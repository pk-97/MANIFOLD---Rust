//! GPU value proofs for the FFT water solver's spectral atoms
//! (docs/FFT_WATER_SOLVER_DESIGN.md P0) against CPU f64 references, plus the
//! box-solve timing the P0 kill check reads.

use std::time::Instant;

use manifold_core::{Beats, Seconds};
use manifold_gpu::GpuBuffer;

use super::cosine_poisson_divide::CosinePoissonDivide;
use super::cosine_reorder::CosineReorder;
use super::cosine_half_spectrum::CosineHalfSpectrum;
use super::cosine_spectrum::CosineSpectrum;
use super::cosine_surface_scale::CosineSurfaceScale;
use super::fft_3d::{Fft3d, InverseFft3d};
use super::liquid_surface_tests::{Harness, params, read};
use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use crate::node_graph::backend::Backend;
use crate::node_graph::bindings::{NodeInputs, NodeOutputs, Slot};
use crate::node_graph::effect_node::{EffectNodeContext, FrameTime, ParamValues};
use crate::node_graph::primitive::Primitive;

fn random_values(count: usize, seed: u64) -> Vec<f32> {
    let mut state = seed | 1;
    (0..count)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 40) as f32 / (1u64 << 24) as f32 - 0.5
        })
        .collect()
}

fn lattice_params(nodes: [usize; 3], extra: &[(&'static str, f32)]) -> ParamValues {
    let mut all = vec![("nodes_x", nodes[0] as f32), ("nodes_y", nodes[1] as f32), ("nodes_z", nodes[2] as f32)];
    all.extend_from_slice(extra);
    params(&all)
}

/// f64 unnormalised DCT-II along the first `axes` axes: Σ_n x[n] Π cos(π k (2n + 1) / 2N).
fn reference_dct(values: &[f32], nodes: [usize; 3], axes: usize) -> Vec<f64> {
    let mut data: Vec<f64> = values.iter().map(|&v| f64::from(v)).collect();
    let stride = [1, nodes[0], nodes[0] * nodes[1]];
    for axis in 0..axes {
        let n = nodes[axis];
        let mut next = vec![0.0; data.len()];
        for (idx, slot) in next.iter_mut().enumerate() {
            let k = (idx / stride[axis]) % n;
            let base = idx - k * stride[axis];
            *slot = (0..n)
                .map(|m| {
                    data[base + m * stride[axis]]
                        * (std::f64::consts::PI * k as f64 * (2 * m + 1) as f64 / (2 * n) as f64).cos()
                })
                .sum();
        }
        data = next;
    }
    data
}

/// f64 inverse of [`reference_dct`] along the first `axes` axes:
/// x[n] = (X[0] + 2 Σ_{k ≥ 1} X[k] cos(π k (2n + 1) / 2N)) / N.
fn reference_idct(coeffs: &[f64], nodes: [usize; 3], axes: usize) -> Vec<f64> {
    let mut data = coeffs.to_vec();
    let stride = [1, nodes[0], nodes[0] * nodes[1]];
    for axis in 0..axes {
        let n = nodes[axis];
        let mut next = vec![0.0; data.len()];
        for (idx, slot) in next.iter_mut().enumerate() {
            let m = (idx / stride[axis]) % n;
            let base = idx - m * stride[axis];
            *slot = (0..n)
                .map(|k| {
                    let weight = if k == 0 { 1.0 } else { 2.0 };
                    weight
                        * data[base + k * stride[axis]]
                        * (std::f64::consts::PI * k as f64 * (2 * m + 1) as f64 / (2 * n) as f64).cos()
                })
                .sum::<f64>()
                / n as f64;
        }
        data = next;
    }
    data
}

/// The helper symbol cosine_surface_scale multiplies coefficient (kx, ky) by.
fn surface_symbol(k: [usize; 2], n: [usize; 2], h: f64, lowest_wave: f64) -> f64 {
    let s = |k: usize, n: usize| (std::f64::consts::FRAC_PI_2 * k as f64 / n as f64).sin();
    (4.0 * (s(k[0], n[0]).powi(2) + s(k[1], n[1]).powi(2)) / (h * h) + lowest_wave * lowest_wave).sqrt()
}

/// What sits between the forward and inverse transforms.
#[derive(Clone, Copy)]
enum Middle {
    Nothing,
    Poisson,
    Surface { lowest_wave: f32 },
}

/// Every atom of the forward and inverse transforms, run in ONE encoder.
struct Chain {
    reorder: CosineReorder,
    fft: Fft3d,
    spectrum: CosineSpectrum,
    divide: CosinePoissonDivide,
    surface: CosineSurfaceScale,
    half: CosineHalfSpectrum,
    ifft: InverseFft3d,
    unorder: CosineReorder,
}

struct ChainSlots {
    input: (Slot, GpuBuffer),
    reordered: (Slot, GpuBuffer),
    spectrum: (Slot, GpuBuffer),
    coeffs: (Slot, GpuBuffer),
    divided: (Slot, GpuBuffer),
    half: (Slot, GpuBuffer),
    back: (Slot, GpuBuffer),
    output: (Slot, GpuBuffer),
}

impl ChainSlots {
    fn new(harness: &mut Harness, values: &[f32], nodes: [usize; 3]) -> Self {
        let total: usize = nodes.iter().product();
        let half = (nodes[0] / 2 + 1) * nodes[1] * nodes[2];
        Self {
            input: harness.array(values, total),
            reordered: harness.array::<f32>(&[], total),
            spectrum: harness.array::<[f32; 2]>(&[], half),
            coeffs: harness.array::<f32>(&[], total),
            divided: harness.array::<f32>(&[], total),
            half: harness.array::<[f32; 2]>(&[], half),
            back: harness.array::<f32>(&[], total),
            output: harness.array::<f32>(&[], total),
        }
    }
}

impl Chain {
    fn new() -> Self {
        Self {
            reorder: CosineReorder::new(),
            fft: Fft3d::new(),
            spectrum: CosineSpectrum::new(),
            divide: CosinePoissonDivide::new(),
            surface: CosineSurfaceScale::new(),
            half: CosineHalfSpectrum::new(),
            ifft: InverseFft3d::new(),
            unorder: CosineReorder::new(),
        }
    }

    /// Encode the chain `repeats` times into one command buffer, transforming
    /// the first `axes` axes, with `middle` between the two transforms.
    /// Returns the wall time and node errors.
    #[allow(clippy::too_many_arguments)]
    fn run(
        &mut self,
        harness: &mut Harness,
        slots: &ChainSlots,
        nodes: [usize; 3],
        cell_size: f32,
        middle: Middle,
        axes: usize,
        repeats: usize,
    ) -> (f64, Vec<String>) {
        let axes = axes as f32;
        let lattice = lattice_params(nodes, &[("axes", axes)]);
        let forward = lattice_params(nodes, &[("direction", 0.0), ("axes", axes)]);
        let inverse = lattice_params(nodes, &[("direction", 1.0), ("axes", axes)]);
        let divide = lattice_params(nodes, &[("cell_size", cell_size)]);
        let lowest_wave = match middle {
            Middle::Surface { lowest_wave } => lowest_wave,
            _ => 0.0,
        };
        let surface = lattice_params(nodes, &[("cell_size", cell_size), ("lowest_wave", lowest_wave)]);
        let middle_slot = match middle {
            Middle::Nothing => slots.coeffs.0,
            Middle::Poisson | Middle::Surface { .. } => slots.divided.0,
        };
        let mut errors = Vec::new();
        let mut native = harness.device.create_encoder("swash chain");
        let start = Instant::now();
        {
            let mut gpu = RendererGpuEncoder::new(&mut native, &harness.device);
            let backend: &dyn Backend = &harness.backend;
            let e = &mut errors;
            for _ in 0..repeats {
                step(&mut self.reorder, &mut gpu, backend, e, ("values", slots.input.0), ("out", slots.reordered.0), &forward);
                step(&mut self.fft, &mut gpu, backend, e, ("values", slots.reordered.0), ("spectrum", slots.spectrum.0), &lattice);
                step(&mut self.spectrum, &mut gpu, backend, e, ("spectrum", slots.spectrum.0), ("out", slots.coeffs.0), &lattice);
                match middle {
                    Middle::Nothing => {}
                    Middle::Poisson => {
                        step(&mut self.divide, &mut gpu, backend, e, ("values", slots.coeffs.0), ("out", slots.divided.0), &divide)
                    }
                    Middle::Surface { .. } => {
                        step(&mut self.surface, &mut gpu, backend, e, ("values", slots.coeffs.0), ("out", slots.divided.0), &surface)
                    }
                }
                step(&mut self.half, &mut gpu, backend, e, ("values", middle_slot), ("spectrum", slots.half.0), &lattice);
                step(&mut self.ifft, &mut gpu, backend, e, ("spectrum", slots.half.0), ("values", slots.back.0), &lattice);
                step(&mut self.unorder, &mut gpu, backend, e, ("values", slots.back.0), ("out", slots.output.0), &inverse);
            }
        }
        native.commit_and_wait_completed();
        (start.elapsed().as_secs_f64() * 1000.0, errors)
    }
}

/// One atom's `run()` into an already-open encoder.
fn step<P: Primitive>(
    prim: &mut P,
    gpu: &mut RendererGpuEncoder<'_>,
    backend: &dyn Backend,
    errors: &mut Vec<String>,
    input: (&'static str, Slot),
    output: (&'static str, Slot),
    step_params: &ParamValues,
) {
    let generations = [0_u64; 64];
    let inputs = [input];
    let outputs = [output];
    let (mut scalars, mut camera, mut light, mut material, mut transform) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut atmosphere, mut render_mode, mut object) = (Vec::new(), Vec::new(), Vec::new());
    let node_inputs = NodeInputs::new(&inputs, backend, &generations);
    let node_outputs = NodeOutputs::new(
        &outputs,
        backend,
        &mut scalars,
        &mut camera,
        &mut light,
        &mut material,
        &mut transform,
        &mut atmosphere,
        &mut render_mode,
        &mut object,
    );
    let time = FrameTime { beats: Beats(0.0), seconds: Seconds(0.0), delta: Seconds(1.0 / 60.0), frame_count: 0 };
    let mut ctx = EffectNodeContext::new(time, step_params, node_inputs, node_outputs, Some(gpu)).with_errors(errors);
    Primitive::run(prim, &mut ctx);
}

#[test]
fn swash_cosine_transform_matches_reference_and_round_trips() {
    let mut harness = Harness::new();
    let nodes = [16usize, 8, 4];
    let total: usize = nodes.iter().product();
    let values = random_values(total, 0x5eed_c05e);
    let slots = ChainSlots::new(&mut harness, &values, nodes);
    let (_, errors) = Chain::new().run(&mut harness, &slots, nodes, 1.0, Middle::Nothing, 3, 1);
    assert!(errors.is_empty(), "{errors:?}");

    let expected = reference_dct(&values, nodes, 3);
    let actual: Vec<f32> = read(&slots.coeffs.1, total);
    let scale = expected.iter().fold(0.0_f64, |m, v| m.max(v.abs()));
    let worst = actual.iter().zip(&expected).map(|(a, e)| (f64::from(*a) - e).abs()).fold(0.0, f64::max);
    assert!(worst < 1e-5 * scale.max(1.0), "cosine transform off by {worst} (scale {scale})");

    let back: Vec<f32> = read(&slots.output.1, total);
    let err = back.iter().zip(&values).map(|(a, b)| (a - b).abs()).fold(0.0_f32, f32::max);
    assert!(err < 1e-5, "forward then inverse does not return the input: {err}");
}

/// Plane mode: every z slice transformed on its own along x and y. The batch
/// count (6) is not a power of two; only transformed lengths need to be.
#[test]
fn swash_plane_transform_matches_reference_and_round_trips() {
    let mut harness = Harness::new();
    let nodes = [16usize, 8, 6];
    let total: usize = nodes.iter().product();
    let values = random_values(total, 0x0091_a4e5);
    let slots = ChainSlots::new(&mut harness, &values, nodes);
    let (_, errors) = Chain::new().run(&mut harness, &slots, nodes, 1.0, Middle::Nothing, 2, 1);
    assert!(errors.is_empty(), "{errors:?}");

    let expected = reference_dct(&values, nodes, 2);
    let actual: Vec<f32> = read(&slots.coeffs.1, total);
    let scale = expected.iter().fold(0.0_f64, |m, v| m.max(v.abs()));
    let worst = actual.iter().zip(&expected).map(|(a, e)| (f64::from(*a) - e).abs()).fold(0.0, f64::max);
    assert!(worst < 1e-5 * scale.max(1.0), "plane transform off by {worst} (scale {scale})");

    let back: Vec<f32> = read(&slots.output.1, total);
    let err = back.iter().zip(&values).map(|(a, b)| (a - b).abs()).fold(0.0_f32, f32::max);
    assert!(err < 1e-5, "plane forward then inverse does not return the input: {err}");
}

/// cosine_surface_scale alone, against the symbol computed on the CPU.
#[test]
fn swash_surface_scale_matches_symbol() {
    let mut harness = Harness::new();
    let nodes = [16usize, 8, 3];
    let total: usize = nodes.iter().product();
    let (h, lowest_wave) = (0.0625_f32, 1.5707964_f32);
    let values = random_values(total, 0x5ca1e);
    let input = harness.array(&values, total);
    let output = harness.array::<f32>(&[], total);
    let surface = lattice_params(nodes, &[("cell_size", h), ("lowest_wave", lowest_wave)]);
    let mut errors = Vec::new();
    let mut native = harness.device.create_encoder("swash surface scale");
    {
        let mut gpu = RendererGpuEncoder::new(&mut native, &harness.device);
        let backend: &dyn Backend = &harness.backend;
        step(&mut CosineSurfaceScale::new(), &mut gpu, backend, &mut errors, ("values", input.0), ("out", output.0), &surface);
    }
    native.commit_and_wait_completed();
    assert!(errors.is_empty(), "{errors:?}");
    let actual: Vec<f32> = read(&output.1, total);
    for (idx, (&a, &v)) in actual.iter().zip(&values).enumerate() {
        let k = [idx % nodes[0], (idx / nodes[0]) % nodes[1]];
        let e = f64::from(v) * surface_symbol(k, [nodes[0], nodes[1]], f64::from(h), f64::from(lowest_wave));
        assert!((f64::from(a) - e).abs() < 1e-5 * e.abs().max(1.0), "entry {idx}: {a} vs {e}");
    }
}

/// The helper's surface operator on a stack of planes: forward plane
/// transform, cosine_surface_scale, inverse, against the same in f64.
#[test]
fn swash_surface_helper_matches_reference() {
    let mut harness = Harness::new();
    let nodes = [16usize, 16, 3];
    let total: usize = nodes.iter().product();
    let (h, lowest_wave) = (0.25_f32, 1.5707964_f32);
    let values = random_values(total, 0x4e1f);
    let slots = ChainSlots::new(&mut harness, &values, nodes);
    let (_, errors) = Chain::new().run(&mut harness, &slots, nodes, h, Middle::Surface { lowest_wave }, 2, 1);
    assert!(errors.is_empty(), "{errors:?}");

    let mut coeffs = reference_dct(&values, nodes, 2);
    for (idx, c) in coeffs.iter_mut().enumerate() {
        let k = [idx % nodes[0], (idx / nodes[0]) % nodes[1]];
        *c *= surface_symbol(k, [nodes[0], nodes[1]], f64::from(h), f64::from(lowest_wave));
    }
    let expected = reference_idct(&coeffs, nodes, 2);
    let actual: Vec<f32> = read(&slots.output.1, total);
    let scale = expected.iter().fold(0.0_f64, |m, v| m.max(v.abs()));
    let worst = actual.iter().zip(&expected).map(|(a, e)| (f64::from(*a) - e).abs()).fold(0.0, f64::max);
    assert!(worst < 1e-5 * scale.max(1.0), "surface helper off by {worst} (scale {scale})");
}

/// Apply the walled 7-point Laplacian (missing neighbours contribute nothing).
fn walled_laplacian(p: &[f32], nodes: [usize; 3], h: f64) -> Vec<f64> {
    let at = |x: usize, y: usize, z: usize| f64::from(p[x + nodes[0] * (y + nodes[1] * z)]);
    let mut out = Vec::with_capacity(p.len());
    for z in 0..nodes[2] {
        for y in 0..nodes[1] {
            for x in 0..nodes[0] {
                let c = at(x, y, z);
                let mut sum = 0.0;
                let pos = [x, y, z];
                for axis in 0..3 {
                    for dir in [-1_i64, 1] {
                        let q = pos[axis] as i64 + dir;
                        if q < 0 || q >= nodes[axis] as i64 {
                            continue;
                        }
                        let mut n = pos;
                        n[axis] = q as usize;
                        sum += at(n[0], n[1], n[2]) - c;
                    }
                }
                out.push(sum / (h * h));
            }
        }
    }
    out
}

#[test]
fn swash_box_solve_inverts_the_walled_laplacian() {
    let mut harness = Harness::new();
    let nodes = [32usize, 16, 8];
    let total: usize = nodes.iter().product();
    let h = 0.0625_f32;
    let mut values = random_values(total, 0xb0c5_5017);
    let mean = values.iter().map(|&v| f64::from(v)).sum::<f64>() / total as f64;
    for v in &mut values {
        *v -= mean as f32;
    }
    let slots = ChainSlots::new(&mut harness, &values, nodes);
    let (_, errors) = Chain::new().run(&mut harness, &slots, nodes, h, Middle::Poisson, 3, 1);
    assert!(errors.is_empty(), "{errors:?}");
    let p: Vec<f32> = read(&slots.output.1, total);
    let lap = walled_laplacian(&p, nodes, f64::from(h));
    let norm = values.iter().map(|&v| f64::from(v).powi(2)).sum::<f64>().sqrt();
    let resid = lap.iter().zip(&values).map(|(l, &f)| (l - f64::from(f)).powi(2)).sum::<f64>().sqrt();
    assert!(resid / norm < 1e-4, "relative residual {}", resid / norm);
    let p_mean = p.iter().map(|&v| f64::from(v)).sum::<f64>() / total as f64;
    assert!(p_mean.abs() < 1e-5, "pressure mean {p_mean}");
}

/// Where the box-solve time goes: the vendor FFT call alone versus one
/// codegen twiddle atom, at a tiny and a real lattice. Equal times at 16³ and
/// 64³ mean fixed per-call overhead, not bandwidth.
#[test]
fn swash_box_solve_cost_split() {
    let mut harness = Harness::new();
    for n in [16usize, 64] {
        let nodes = [n; 3];
        let values = random_values(n * n * n, 0x5b1d);
        let slots = ChainSlots::new(&mut harness, &values, nodes);
        let lattice = lattice_params(nodes, &[]);
        let mut fft = Fft3d::new();
        let mut spectrum = CosineSpectrum::new();
        let repeats = 100;
        let mut reorder = CosineReorder::new();
        let forward = lattice_params(nodes, &[("direction", 0.0)]);
        for (label, which) in [("fft_3d", 0), ("cosine_spectrum", 1), ("cosine_reorder", 2)] {
            for round in 0..2 {
                let mut errors = Vec::new();
                let mut native = harness.device.create_encoder("swash cost split");
                let start = Instant::now();
                {
                    let mut gpu = RendererGpuEncoder::new(&mut native, &harness.device);
                    let backend: &dyn Backend = &harness.backend;
                    for _ in 0..repeats {
                        if which == 0 {
                            step(&mut fft, &mut gpu, backend, &mut errors, ("values", slots.input.0), ("spectrum", slots.spectrum.0), &lattice);
                        } else if which == 1 {
                            step(&mut spectrum, &mut gpu, backend, &mut errors, ("spectrum", slots.spectrum.0), ("out", slots.coeffs.0), &lattice);
                        } else {
                            step(&mut reorder, &mut gpu, backend, &mut errors, ("values", slots.input.0), ("out", slots.reordered.0), &forward);
                        }
                    }
                }
                let encoded = start.elapsed().as_secs_f64();
                native.commit_and_wait_completed();
                assert!(errors.is_empty(), "{errors:?}");
                if round == 1 {
                    let per = |s: f64| s * 1e6 / repeats as f64;
                    let total = start.elapsed().as_secs_f64();
                    println!(
                        "SWASH cost split {n}³ {label}: {:.1} µs per call ({:.1} CPU encode, {:.1} GPU after commit)",
                        per(total),
                        per(encoded),
                        per(total - encoded)
                    );
                }
            }
        }
    }
}

/// The P0 kill check's number: one full box solve at 64³ and 128³.
#[test]
fn swash_box_solve_timing() {
    let mut harness = Harness::new();
    for n in [64usize, 128] {
        let nodes = [n; 3];
        let values = random_values(n * n * n, 0x7117);
        let slots = ChainSlots::new(&mut harness, &values, nodes);
        let mut chain = Chain::new();
        let (_, errors) = chain.run(&mut harness, &slots, nodes, 1.0, Middle::Poisson, 3, 2);
        assert!(errors.is_empty(), "{errors:?}");
        let repeats = 50;
        let (ms, errors) = chain.run(&mut harness, &slots, nodes, 1.0, Middle::Poisson, 3, repeats);
        assert!(errors.is_empty(), "{errors:?}");
        println!("SWASH box solve {n}³: {:.3} ms per solve ({repeats} solves in one command buffer)", ms / repeats as f64);
    }
}
