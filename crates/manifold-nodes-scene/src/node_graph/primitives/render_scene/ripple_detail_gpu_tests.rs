//! GPU proofs for the ripple detail: the slopes kernel against a sine's
//! derivative, a real tile from the ocean spectrum, and the crossfade of the
//! look being invisible at its swap.
use super::super::water_march_gpu_tests::shared_buffer;
use super::super::water_shade::ripple_uniforms;
use super::super::{FIELD_TILES_WGSL, model_matrix};
use super::*;
use crate::testkit::field_tiles::FieldTable;
use half::f16;
use manifold_node_engine::scene::world_tiles::READ_SIDE;

const N: usize = RIPPLE_TEXELS as usize;
/// The display cell in metres: the ripple tile is 32 of them, 1.6 m.
pub(super) const CELL: f32 = 0.05;

pub(super) fn read_f32(buffer: &GpuBuffer, count: usize) -> Vec<f32> {
    let ptr = buffer.mapped_ptr().expect("shared buffer");
    unsafe { std::slice::from_raw_parts(ptr as *const f32, count) }.to_vec()
}

/// Mip 0 of `texture` read back as (sx, sz) per texel, row-major.
fn read_slopes(device: &GpuDevice, texture: &GpuTexture) -> Vec<[f32; 2]> {
    let out = device.create_buffer_shared((N * N * 4) as u64);
    let mut enc = device.create_encoder("ripple slopes readback");
    enc.copy_texture_to_buffer(texture, &out, N as u32, N as u32, N as u32 * 4);
    enc.commit_and_wait_completed();
    let halves: &[u16] = unsafe {
        std::slice::from_raw_parts(out.mapped_ptr().expect("shared").cast::<u16>(), N * N * 2)
    };
    halves
        .chunks_exact(2)
        .map(|c| [f16::from_bits(c[0]).to_f32(), f16::from_bits(c[1]).to_f32()])
        .collect()
}

/// A sine in x through the slopes kernel alone: sx is its derivative, sz is zero.
#[test]
fn slopes_of_a_sine_are_its_derivative() {
    let device = manifold_gpu::testkit::test_device();
    let (amplitude, length) = (0.01f32, 1.6f32);
    let k = std::f32::consts::TAU * 3.0 / length;
    let heights: Vec<f32> = (0..N * N)
        .map(|i| {
            let x = (i % N) as f32 * length / N as f32;
            amplitude * (k * x).sin()
        })
        .collect();
    let buffer = device.create_buffer_shared((N * N * 4) as u64);
    unsafe {
        buffer.write(0, bytemuck::cast_slice(&heights));
    }
    let texture = device.create_texture(&GpuTextureDesc {
        width: N as u32,
        height: N as u32,
        depth: 1,
        mip_levels: 1,
        format: GpuTextureFormat::Rg16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_READ
            | GpuTextureUsage::SHADER_WRITE
            | GpuTextureUsage::COPY_SRC,
        label: "ripple slopes test",
    });
    let pipeline = device.create_compute_pipeline(SLOPES_WGSL, "cs_slopes", "ripple slopes test");
    let mut enc = device.create_encoder("ripple slopes test");
    encode_slopes(&mut enc, &pipeline, &buffer, &texture, length);
    enc.commit_and_wait_completed();

    let slopes = read_slopes(&device, &texture);
    let peak = amplitude * k;
    for (i, s) in slopes.iter().enumerate() {
        let x = (i % N) as f32 * length / N as f32;
        let want = peak * (k * x).cos();
        assert!(
            (s[0] - want).abs() <= 0.005 * peak,
            "texel {i}: sx {} against {want} (peak {peak})",
            s[0]
        );
        assert!(s[1].abs() <= 1e-6, "texel {i}: sz {}", s[1]);
    }
}

pub(super) fn identity() -> [[f32; 4]; 4] {
    model_matrix([0.0; 3], [0.0; 3], [1.0; 3])
}

/// The tiles of the test field: 3 x 1 x 3 from the origin, covering the probe
/// points.
pub(super) fn table() -> FieldTable {
    FieldTable::new(CELL, 9, &FieldTable::tiles_between([0, 0, 0], [2, 0, 2]))
}

/// The buffers of the test field: a level set the ripples never read, the
/// velocity (`flow` at every held node) and the table with its extent.
pub(super) struct Lattices {
    pub(super) table: FieldTable,
    pub(super) levelset: GpuBuffer,
    pub(super) velocity: GpuBuffer,
    pub(super) tiles: GpuBuffer,
    pub(super) extent: GpuBuffer,
}

pub(super) fn lattices(device: &GpuDevice, flow: [f32; 3]) -> Lattices {
    let table = table();
    let velocity = table.pool([0.0; 3], |_| flow);
    Lattices {
        levelset: device.create_buffer_shared(16),
        velocity: shared_buffer(device, &velocity),
        tiles: shared_buffer(device, &table.words),
        extent: shared_buffer(device, &table.extent()),
        table,
    }
}

pub(super) fn field(lattices: &Lattices) -> FieldObject<'_> {
    FieldObject {
        slot: 0,
        levelset: &lattices.levelset,
        velocity: Some(&lattices.velocity),
        foam: None,
        mist: None,
        wet_map: None,
        tiles: &lattices.tiles,
        extent: &lattices.extent,
        pool_tiles: lattices.table.pool_tiles,
        cell_size: CELL,
        model: identity(),
        prev_model: identity(),
        ripple_detail: 1.0,
        uniforms: bytemuck::Zeroable::zeroed(),
        mesh: None,
    }
}

/// A tile made by `RippleDetail::encode` for one field, finished on the GPU.
pub(super) fn made_tile(device: &GpuDevice, field: &FieldObject) -> RippleDetail {
    let mut ripples = RippleDetail::default();
    let mut enc = device.create_encoder("ripple tile");
    ripples.encode(device, &mut enc, std::slice::from_ref(field), 2.0);
    enc.commit_and_wait_completed();
    ripples
}

/// A tile from the ocean spectrum has ripples of a physical size and repeats seamlessly.
#[test]
fn a_tile_has_ripples_and_repeats() {
    let device = manifold_gpu::testkit::test_device();
    let lattices = lattices(&device, [0.8, 0.0, 0.3]);
    let field = field(&lattices);
    let ripples = made_tile(&device, &field);
    let tile = ripples.tile(0).expect("the field wants ripples");
    assert!((tile.tile_metres - 1.6).abs() < 1e-5);
    let slopes = read_slopes(&device, &tile.texture);

    let sum_sq: f64 = slopes
        .iter()
        .map(|s| f64::from(s[0]).powi(2) + f64::from(s[1]).powi(2))
        .sum();
    let rms = (sum_sq / (2 * N * N) as f64).sqrt();
    assert!(
        slopes.iter().any(|s| s[0] != 0.0 || s[1] != 0.0),
        "all slopes are zero"
    );
    assert!((0.001..1.0).contains(&rms), "slope RMS {rms}");

    for channel in [0usize, 1] {
        let at = |x: usize, z: usize| slopes[z * N + x][channel];
        let mut inside = 0.0f32;
        let mut across = 0.0f32;
        for z in 0..N {
            for x in 0..N {
                if x + 1 < N {
                    inside = inside.max((at(x + 1, z) - at(x, z)).abs());
                }
                if z + 1 < N {
                    inside = inside.max((at(x, z + 1) - at(x, z)).abs());
                }
            }
        }
        for i in 0..N {
            across = across
                .max((at(0, i) - at(N - 1, i)).abs())
                .max((at(i, 0) - at(i, N - 1)).abs());
        }
        assert!(
            across <= inside,
            "channel {channel}: the wrap steps by {across}, the largest step inside is {inside}"
        );
    }
}

pub(super) const PROBE_STUB: &str = "struct SceneStub { camera_pos: vec4<f32> };\n\
@group(0) @binding(0) var<uniform> u: SceneStub;\n";

/// Evaluates the look at 64 x 64 points on y = 0 with the normal up, over the
/// held tiles.
const PROBE_KERNEL: &str = "\n\
@group(0) @binding(90) var<storage, read_write> probe_out: array<vec4<f32>>;\n\
@compute @workgroup_size(64)\n\
fn cs_probe(@builtin(global_invocation_id) id: vec3<u32>) {\n\
    let i = id.x;\n\
    if i >= arrayLength(&probe_out) { return; }\n\
    let p = vec3<f32>(f32(i % 64u) * 0.0155 + 0.05, 0.0, f32(i / 64u) * 0.0145 + 0.03);\n\
    probe_out[i] = vec4<f32>(look_ripple_detail(p, vec3<f32>(0.0, 1.0, 0.0)), 0.0);\n\
}\n";

pub(super) const PROBES: usize = 4096;

/// The look's normals at the probe points at each of `times`, with the foam
/// field `foam` bound when there is one.
pub(super) fn probe_normals(
    device: &GpuDevice,
    ripples: &RippleDetail,
    field: &FieldObject,
    foam: Option<&GpuBuffer>,
    times: &[f32],
) -> Vec<Vec<[f32; 3]>> {
    let velocity = field.velocity.expect("the field has a velocity array");
    let wgsl = format!(
        "{READ_SIDE}{FIELD_TILES_WGSL}{PROBE_STUB}{}{}{PROBE_KERNEL}",
        include_str!("../shaders/look_ripple_detail.wgsl"),
        include_str!("../shaders/foam_field.wgsl"),
    );
    let pipeline = device.create_compute_pipeline(&wgsl, "cs_probe", "ripple probe");
    let tile = ripples.tile(field.slot).expect("the field has a tile");
    let camera = [0.0f32, 3.0, 0.0, 1.0];
    let outs: Vec<GpuBuffer> = times
        .iter()
        .map(|_| device.create_buffer_shared((PROBES * 16) as u64))
        .collect();
    let mut enc = device.create_encoder("ripple probe");
    for (&time, out) in times.iter().zip(&outs) {
        // A pixel is far smaller than a texel, so the finest mip is read.
        let uniforms = ripple_uniforms(
            field,
            Some(tile.tile_metres),
            foam.is_some(),
            time,
            [0.001, 0.0],
        )
        .expect("a real model inverts");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&camera),
                },
                GpuBinding::Texture {
                    binding: 62,
                    texture: &tile.texture,
                },
                GpuBinding::Sampler {
                    binding: 63,
                    sampler: ripples.sampler(),
                },
                GpuBinding::Bytes {
                    binding: 64,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 65,
                    buffer: velocity,
                    offset: 0,
                },
                // Without a foam field the stand-in is never read.
                GpuBinding::Buffer {
                    binding: 66,
                    buffer: match foam {
                        Some(foam) => foam,
                        None => velocity,
                    },
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 70,
                    buffer: field.tiles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 90,
                    buffer: out,
                    offset: 0,
                },
            ],
            [(PROBES / 64) as u32, 1, 1],
            "ripple probe",
        );
    }
    enc.commit_and_wait_completed();
    outs.iter()
        .map(|out| {
            read_f32(out, PROBES * 4)
                .chunks_exact(4)
                .map(|c| [c[0], c[1], c[2]])
                .collect()
        })
        .collect()
}

pub(super) fn angle(a: [f32; 3], b: [f32; 3]) -> f64 {
    let (a, b) = (a.map(f64::from), b.map(f64::from));
    let cross = [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ];
    let cross_len = (cross[0] * cross[0] + cross[1] * cross[1] + cross[2] * cross[2]).sqrt();
    cross_len.atan2(a[0] * b[0] + a[1] * b[1] + a[2] * b[2])
}

fn mean_change(before: &[[f32; 3]], after: &[[f32; 3]]) -> f64 {
    before
        .iter()
        .zip(after)
        .map(|(a, b)| angle(*a, *b))
        .sum::<f64>()
        / before.len() as f64
}

/// The two copies swap at whole and half cycles; the normals change no faster
/// there than mid-cycle (the blend's norm moves by at most 4 dt / T), and the
/// ripples are really there.
#[test]
fn the_crossfade_swap_is_not_seen() {
    let device = manifold_gpu::testkit::test_device();
    let lattices = lattices(&device, [0.8, 0.0, 0.3]);
    let field = field(&lattices);
    let ripples = made_tile(&device, &field);

    let dt = 1.0f32 / 60.0;
    let period = FLOW_PERIOD_SECONDS;
    let centres = [2.0f32, 2.5, 2.25, 2.75];
    let times: Vec<f32> = centres
        .iter()
        .flat_map(|t| [t - dt / 2.0, t + dt / 2.0])
        .collect();
    let normals = probe_normals(&device, &ripples, &field, None, &times);

    let change = |i: usize| mean_change(&normals[2 * i], &normals[2 * i + 1]);
    let swap = (change(0) + change(1)) / 2.0;
    let mid = (change(2) + change(3)) / 2.0;
    let bound = (1.0 + f64::from(4.0 * dt / period)) * mid;
    assert!(mid > 0.0, "the ripples do not move mid-cycle");
    assert!(
        swap <= bound,
        "mean change at a swap {swap} rad against {mid} rad mid-cycle (bound {bound})"
    );

    let tilt = normals[4]
        .iter()
        .map(|n| angle(*n, [0.0, 1.0, 0.0]))
        .sum::<f64>()
        / PROBES as f64;
    assert!(
        tilt.to_degrees() > 0.5,
        "mean tilt {} degrees",
        tilt.to_degrees()
    );
}
