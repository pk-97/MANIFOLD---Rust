//! GPU value proofs for the FFT water solver's spectral atoms
//! (docs/FFT_WATER_SOLVER_DESIGN.md P0) against CPU f64 references, plus the
//! box-solve timing the P0 kill check reads.

use std::time::Instant;

use manifold_core::{Beats, Seconds};
use manifold_gpu::GpuBuffer;

use super::chart_entries::ChartEntries;
use super::chart_spread::ChartSpread;
use super::chart_sums::ChartSums;
use super::collar_cells::CollarCells;
use super::collar_gather::CollarGather;
use super::collar_pressure::CollarPressure;
use super::collar_source::CollarSource;
use super::select_flagged::SelectFlagged;
use crate::node_graph::fluid_particles::ChartEntry;
use super::cosine_poisson_divide::CosinePoissonDivide;
use super::cosine_reorder::CosineReorder;
use super::cosine_half_spectrum::CosineHalfSpectrum;
use super::cosine_spectrum::CosineSpectrum;
use super::combine_rows::CombineRows;
use super::cosine_surface_scale::CosineSurfaceScale;
use super::divide_by_value::DivideByValue;
use super::dot_products::DotProducts;
use super::krylov_givens::{KrylovGivens, MAX_PASSES, residual_offset, state_len};
use super::krylov_solve::KrylovSolve;
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
    /// Returns the command buffer's GPU ms and node errors.
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
        (native.commit_and_wait_completed_timed() * 1000.0, errors)
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
    step_ports(prim, gpu, backend, errors, &[input], &[output], step_params);
}

/// One atom's `run()` with several array ports.
fn step_ports<P: Primitive>(
    prim: &mut P,
    gpu: &mut RendererGpuEncoder<'_>,
    backend: &dyn Backend,
    errors: &mut Vec<String>,
    inputs: &[(&'static str, Slot)],
    outputs: &[(&'static str, Slot)],
    step_params: &ParamValues,
) {
    let generations = [0_u64; 64];
    let (mut scalars, mut camera, mut light, mut material, mut transform) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut atmosphere, mut render_mode, mut object) = (Vec::new(), Vec::new(), Vec::new());
    let node_inputs = NodeInputs::new(inputs, backend, &generations);
    let node_outputs = NodeOutputs::new(
        outputs,
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

/// A power-of-two lattice and a mixed-radix one (factors 3 and 5).
#[test]
fn swash_cosine_transform_matches_reference_and_round_trips() {
    for nodes in [[16usize, 8, 4], [12, 10, 6]] {
        let mut harness = Harness::new();
        let total: usize = nodes.iter().product();
        let values = random_values(total, 0x5eed_c05e);
        let slots = ChainSlots::new(&mut harness, &values, nodes);
        let (_, errors) = Chain::new().run(&mut harness, &slots, nodes, 1.0, Middle::Nothing, 3, 1);
        assert!(errors.is_empty(), "{nodes:?}: {errors:?}");

        let expected = reference_dct(&values, nodes, 3);
        let actual: Vec<f32> = read(&slots.coeffs.1, total);
        let scale = expected.iter().fold(0.0_f64, |m, v| m.max(v.abs()));
        let worst = actual.iter().zip(&expected).map(|(a, e)| (f64::from(*a) - e).abs()).fold(0.0, f64::max);
        assert!(worst < 1e-5 * scale.max(1.0), "{nodes:?}: cosine transform off by {worst} (scale {scale})");

        let back: Vec<f32> = read(&slots.output.1, total);
        let err = back.iter().zip(&values).map(|(a, b)| (a - b).abs()).fold(0.0_f32, f32::max);
        assert!(err < 1e-5, "{nodes:?}: forward then inverse does not return the input: {err}");
    }
}

/// Plane mode: every z slice transformed on its own along x and y. The batch
/// count may be anything; the second lattice's planes are mixed radix.
#[test]
fn swash_plane_transform_matches_reference_and_round_trips() {
    for nodes in [[16usize, 8, 6], [10, 6, 5]] {
        let mut harness = Harness::new();
        let total: usize = nodes.iter().product();
        let values = random_values(total, 0x0091_a4e5);
        let slots = ChainSlots::new(&mut harness, &values, nodes);
        let (_, errors) = Chain::new().run(&mut harness, &slots, nodes, 1.0, Middle::Nothing, 2, 1);
        assert!(errors.is_empty(), "{nodes:?}: {errors:?}");

        let expected = reference_dct(&values, nodes, 2);
        let actual: Vec<f32> = read(&slots.coeffs.1, total);
        let scale = expected.iter().fold(0.0_f64, |m, v| m.max(v.abs()));
        let worst = actual.iter().zip(&expected).map(|(a, e)| (f64::from(*a) - e).abs()).fold(0.0, f64::max);
        assert!(worst < 1e-5 * scale.max(1.0), "{nodes:?}: plane transform off by {worst} (scale {scale})");

        let back: Vec<f32> = read(&slots.output.1, total);
        let err = back.iter().zip(&values).map(|(a, b)| (a - b).abs()).fold(0.0_f32, f32::max);
        assert!(err < 1e-5, "{nodes:?}: plane forward then inverse does not return the input: {err}");
    }
}

/// cosine_surface_scale alone, against the symbol computed on the CPU, with
/// and without the helper's 2/h offset.
#[test]
fn swash_surface_scale_matches_symbol() {
    let mut harness = Harness::new();
    let nodes = [16usize, 8, 3];
    let total: usize = nodes.iter().product();
    let (h, lowest_wave) = (0.0625_f32, 1.5707964_f32);
    let values = random_values(total, 0x5ca1e);
    let input = harness.array(&values, total);
    let output = harness.array::<f32>(&[], total);
    for offset in [0.0, 2.0 / h] {
        let surface = lattice_params(nodes, &[("cell_size", h), ("lowest_wave", lowest_wave), ("offset", offset)]);
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
            let symbol = surface_symbol(k, [nodes[0], nodes[1]], f64::from(h), f64::from(lowest_wave));
            let e = f64::from(v) * (symbol - f64::from(offset));
            assert!((f64::from(a) - e).abs() < 1e-5 * (f64::from(v) * symbol).abs().max(1.0), "entry {idx}: {a} vs {e}");
        }
    }
}

/// The helper's surface operator on a stack of planes: forward plane
/// transform, cosine_surface_scale, inverse, against the same in f64, on
/// square power-of-two planes and on mixed-radix ones.
#[test]
fn swash_surface_helper_matches_reference() {
    for nodes in [[16usize, 16, 3], [12, 20, 3]] {
        let mut harness = Harness::new();
        let total: usize = nodes.iter().product();
        let (h, lowest_wave) = (0.25_f32, 1.5707964_f32);
        let values = random_values(total, 0x4e1f);
        let slots = ChainSlots::new(&mut harness, &values, nodes);
        let (_, errors) = Chain::new().run(&mut harness, &slots, nodes, h, Middle::Surface { lowest_wave }, 2, 1);
        assert!(errors.is_empty(), "{nodes:?}: {errors:?}");

        let mut coeffs = reference_dct(&values, nodes, 2);
        for (idx, c) in coeffs.iter_mut().enumerate() {
            let k = [idx % nodes[0], (idx / nodes[0]) % nodes[1]];
            *c *= surface_symbol(k, [nodes[0], nodes[1]], f64::from(h), f64::from(lowest_wave));
        }
        let expected = reference_idct(&coeffs, nodes, 2);
        let actual: Vec<f32> = read(&slots.output.1, total);
        let scale = expected.iter().fold(0.0_f64, |m, v| m.max(v.abs()));
        let worst = actual.iter().zip(&expected).map(|(a, e)| (f64::from(*a) - e).abs()).fold(0.0, f64::max);
        assert!(worst < 1e-5 * scale.max(1.0), "{nodes:?}: surface helper off by {worst} (scale {scale})");
    }
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

/// On a power-of-two box and a mixed-radix one (factors 3 and 5).
#[test]
fn swash_box_solve_inverts_the_walled_laplacian() {
    for nodes in [[32usize, 16, 8], [24, 20, 12]] {
        let mut harness = Harness::new();
        let total: usize = nodes.iter().product();
        let h = 0.0625_f32;
        let mut values = random_values(total, 0xb0c5_5017);
        let mean = values.iter().map(|&v| f64::from(v)).sum::<f64>() / total as f64;
        for v in &mut values {
            *v -= mean as f32;
        }
        let slots = ChainSlots::new(&mut harness, &values, nodes);
        let (_, errors) = Chain::new().run(&mut harness, &slots, nodes, h, Middle::Poisson, 3, 1);
        assert!(errors.is_empty(), "{nodes:?}: {errors:?}");
        let p: Vec<f32> = read(&slots.output.1, total);
        let lap = walled_laplacian(&p, nodes, f64::from(h));
        let norm = values.iter().map(|&v| f64::from(v).powi(2)).sum::<f64>().sqrt();
        let resid = lap.iter().zip(&values).map(|(l, &f)| (l - f64::from(f)).powi(2)).sum::<f64>().sqrt();
        assert!(resid / norm < 1e-4, "{nodes:?}: relative residual {}", resid / norm);
        let p_mean = p.iter().map(|&v| f64::from(v)).sum::<f64>() / total as f64;
        assert!(p_mean.abs() < 1e-5, "{nodes:?}: pressure mean {p_mean}");
    }
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

/// The P0 kill check's number, one full box solve at 64³ and 128³, beside
/// the clipped boxes a settled Dam Break pool needs (P3c: the water's bounds
/// plus the collar and one 8-cell pad, 24 of 64 and 40 of 128 cells high)
/// and a 16³ box, where the solve is all fixed cost per dispatch.
#[test]
fn swash_box_solve_timing() {
    let mut harness = Harness::new();
    for nodes in [[16usize, 16, 16], [64, 64, 64], [64, 24, 64], [128, 128, 128], [128, 40, 128]] {
        let total: usize = nodes.iter().product();
        let values = random_values(total, 0x7117);
        let slots = ChainSlots::new(&mut harness, &values, nodes);
        let mut chain = Chain::new();
        let (_, errors) = chain.run(&mut harness, &slots, nodes, 1.0, Middle::Poisson, 3, 2);
        assert!(errors.is_empty(), "{errors:?}");
        let repeats = 50;
        let mut ms: Vec<f64> = (0..5).map(|_| chain.run(&mut harness, &slots, nodes, 1.0, Middle::Poisson, 3, repeats).0).collect();
        ms.sort_by(f64::total_cmp);
        println!("SWASH box solve {nodes:?}: {:.3} ms GPU per solve (median of 5 command buffers of {repeats})", ms[2] / repeats as f64);
    }
}

/// Run one atom on fresh arrays and read its output back.
fn run_atom<P: Primitive>(
    prim: &mut P,
    inputs: &[(&'static str, &[f32])],
    output_len: usize,
    step_params: &ParamValues,
) -> Vec<f32> {
    let mut harness = Harness::new();
    let slots: Vec<(&'static str, (Slot, GpuBuffer))> =
        inputs.iter().map(|&(name, values)| (name, harness.array(values, values.len().max(1)))).collect();
    let output = harness.array::<f32>(&[], output_len);
    let ports: Vec<(&'static str, Slot)> = slots.iter().map(|(name, (slot, _))| (*name, *slot)).collect();
    let mut errors = Vec::new();
    let mut native = harness.device.create_encoder("swash atom");
    {
        let mut gpu = RendererGpuEncoder::new(&mut native, &harness.device);
        let backend: &dyn Backend = &harness.backend;
        step_ports(prim, &mut gpu, backend, &mut errors, &ports, &[("out", output.0)], step_params);
    }
    native.commit_and_wait_completed();
    assert!(errors.is_empty(), "{errors:?}");
    read(&output.1, output_len)
}

fn assert_close(actual: &[f32], expected: &[f64], what: &str) {
    let scale = expected.iter().fold(1.0_f64, |m, v| m.max(v.abs()));
    for (i, (a, e)) in actual.iter().zip(expected).enumerate() {
        assert!((f64::from(*a) - e).abs() <= 1e-5 * scale, "{what}[{i}]: {a} vs {e}");
    }
}

#[test]
fn swash_dot_products_match_cpu() {
    let (len, rows) = (3000usize, 5usize);
    let matrix = random_values(len * rows, 0xd07);
    let vector = random_values(len, 0x7ec);
    let dot = |r: usize, v: &[f32]| (0..len).map(|e| f64::from(matrix[r * len + e]) * f64::from(v[e])).sum::<f64>();
    let got = run_atom(
        &mut DotProducts::new(),
        &[("matrix", &matrix), ("vector", &vector)],
        6,
        &params(&[("row_length", len as f32), ("rows", 4.0), ("max_rows", 6.0)]),
    );
    let want: Vec<f64> = (0..6).map(|r| if r < 4 { dot(r, &vector) } else { 0.0 }).collect();
    assert_close(&got, &want, "dots");
    // No vector: each row's plain sum.
    let sums = run_atom(
        &mut DotProducts::new(),
        &[("matrix", &matrix)],
        2,
        &params(&[("row_length", len as f32), ("rows", 2.0), ("max_rows", 2.0)]),
    );
    let ones = vec![1.0_f32; len];
    assert_close(&sums, &[dot(0, &ones), dot(1, &ones)], "sums");
    // A length: the vector against itself, square-rooted.
    let row0 = matrix[..len].to_vec();
    let length = run_atom(
        &mut DotProducts::new(),
        &[("matrix", &row0), ("vector", &row0)],
        1,
        &params(&[("row_length", len as f32), ("rows", 1.0), ("max_rows", 1.0), ("root", 1.0)]),
    );
    assert_close(&length, &[dot(0, &row0).sqrt()], "length");
}

#[test]
fn swash_combine_rows_match_cpu() {
    let (len, rows) = (1000usize, 4usize);
    let base = random_values(len, 0xba5e);
    let matrix = random_values(len * rows, 0x3a7);
    let coef = random_values(rows, 0xc0ef);
    let got = run_atom(
        &mut CombineRows::new(),
        &[("base", &base), ("matrix", &matrix), ("coef", &coef)],
        len,
        &params(&[("row_length", len as f32), ("rows", 3.0), ("scale", -1.0), ("base_scale", 0.5)]),
    );
    let want: Vec<f64> = (0..len)
        .map(|e| {
            0.5 * f64::from(base[e])
                - (0..3).map(|i| f64::from(coef[i]) * f64::from(matrix[i * len + e])).sum::<f64>()
        })
        .collect();
    assert_close(&got, &want, "combine");
}

#[test]
fn swash_divide_by_value_matches_cpu_and_guards_zero() {
    let values = random_values(700, 0xd1f0);
    let got = run_atom(&mut DivideByValue::new(), &[("values", &values), ("divisor", &[0.25])], 700, &params(&[]));
    let want: Vec<f64> = values.iter().map(|&v| f64::from(v) / 0.25).collect();
    assert_close(&got, &want, "divide");
    let zero = run_atom(&mut DivideByValue::new(), &[("values", &values), ("divisor", &[0.0])], 700, &params(&[]));
    assert!(zero.iter().all(|&v| v == 0.0), "a zero divisor must give zeros");
}

/// CPU port of node.krylov_givens for one pass.
fn cpu_givens(state: &[f32], first: &[f32], second: &[f32], norm: f32, m: usize, j: usize) -> Vec<f64> {
    let mut out: Vec<f64> = state.iter().map(|&v| f64::from(v)).collect();
    let (off_c, off_s) = (m * (m + 1), m * (m + 1) + m);
    let off_g = off_s + m;
    let mut col: Vec<f64> = (0..=j).map(|i| f64::from(first[i]) + f64::from(second[i])).collect();
    col.push(f64::from(norm));
    for i in 0..j {
        let (c, s) = (out[off_c + i], out[off_s + i]);
        let t = c * col[i] + s * col[i + 1];
        col[i + 1] = -s * col[i] + c * col[i + 1];
        col[i] = t;
    }
    let r = (col[j] * col[j] + col[j + 1] * col[j + 1]).sqrt();
    let (c, s) = if r > 1e-30 { (col[j] / r, col[j + 1] / r) } else { (1.0, 0.0) };
    for i in 0..=m {
        out[j * (m + 1) + i] = if i < j { col[i] } else if i == j { r } else { 0.0 };
    }
    let g = out[off_g + j];
    out[off_c + j] = c;
    out[off_s + j] = s;
    out[off_g + j] = c * g;
    out[off_g + j + 1] = -s * g;
    out
}

/// A short solve, and one at the kernels' cap, where the local column and
/// coefficient arrays are full.
#[test]
fn swash_krylov_givens_and_solve_match_cpu() {
    for m in [6, MAX_PASSES as usize] {
        krylov_givens_and_solve_match_cpu(m);
    }
}

fn krylov_givens_and_solve_match_cpu(m: usize) {
    let len = state_len(m as u32) as usize;
    // Each pass starts from the CPU's state after the previous one.
    let mut state = vec![0.0f32; len];
    state[residual_offset(m as u32) as usize] = 2.0;
    for j in 0..m {
        let first = random_values(m + 1, 0x100 + j as u64);
        let second: Vec<f32> = random_values(m + 1, 0x200 + j as u64).iter().map(|v| v * 1e-3).collect();
        let norm = 0.3 + 0.1 * j as f32;
        let want = cpu_givens(&state, &first, &second, norm, m, j);
        let got = run_atom(
            &mut KrylovGivens::new(),
            &[("state", &state), ("first", &first), ("second", &second), ("norm", &[norm])],
            len,
            &params(&[("passes", m as f32), ("column", j as f32)]),
        );
        assert_close(&got, &want, &format!("givens pass {j} of {m}"));
        state = want.iter().map(|&v| v as f32).collect();
    }
    let y = run_atom(&mut KrylovSolve::new(), &[("state", &state)], m, &params(&[("passes", m as f32)]));
    let off_g = residual_offset(m as u32) as usize;
    let mut want = vec![0.0f64; m];
    for k in (0..m).rev() {
        let acc = f64::from(state[off_g + k])
            - (k + 1..m).map(|l| f64::from(state[l * (m + 1) + k]) * want[l]).sum::<f64>();
        let d = f64::from(state[k * (m + 1) + k]);
        want[k] = if d.abs() > 1e-30 { acc / d } else { 0.0 };
    }
    assert_close(&y, &want, &format!("solve of {m}"));
}

// The collar atoms (P1) against CPU ports, on a lattice small enough to read
// whole and odd enough that no side is a power of two.

const LATTICE: [usize; 3] = [8, 6, 5];

fn cell_of(p: [usize; 3], n: [usize; 3]) -> usize {
    p[0] + n[0] * (p[1] + n[1] * p[2])
}

fn coords(c: usize, n: [usize; 3]) -> [usize; 3] {
    [c % n[0], (c / n[0]) % n[1], c / (n[0] * n[1])]
}

/// Half the cells water, at random: several runs on most lines.
fn random_water(n: [usize; 3], seed: u64) -> Vec<f32> {
    random_values(n.iter().product(), seed).iter().map(|&v| if v > 0.0 { 1.0 } else { 0.0 }).collect()
}

fn cpu_collar(water: &[f32], n: [usize; 3]) -> Vec<u32> {
    (0..water.len())
        .map(|c| {
            let p = coords(c, n);
            let wet_neighbour = (0..3).any(|a| {
                [p[a].checked_sub(1), Some(p[a] + 1).filter(|&q| q < n[a])].into_iter().flatten().any(|q| {
                    let mut r = p;
                    r[a] = q;
                    water[cell_of(r, n)] > 0.5
                })
            });
            u32::from(water[c] <= 0.5 && wet_neighbour)
        })
        .collect()
}

fn cpu_running_total(flags: &[u32]) -> Vec<u32> {
    let mut acc = 0;
    flags.iter().map(|&f| {
        acc += f;
        acc
    }).collect()
}

/// The flagged cells in order, cut or padded with u32::MAX to `capacity`.
fn cpu_entries(flags: &[u32], capacity: usize) -> Vec<u32> {
    let mut list: Vec<u32> = (0..flags.len() as u32).filter(|&c| flags[c as usize] != 0).collect();
    list.truncate(capacity);
    list.resize(capacity, u32::MAX);
    list
}

fn cpu_sheet(runs: u32, sheets: u32) -> u32 {
    (runs.max(1) - 1).min(sheets - 1)
}

fn cpu_chart_entries(entries: &[u32], water: &[f32], smoothed: &[f32], collar: &[u32], n: [usize; 3], sheets: u32) -> Vec<ChartEntry> {
    entries
        .iter()
        .map(|&c| {
            if c as usize >= water.len() {
                return ChartEntry { cell: u32::MAX, ..Default::default() };
            }
            let p = coords(c as usize, n);
            let grad: [f64; 3] = std::array::from_fn(|a| {
                let (mut hi, mut lo) = (p, p);
                hi[a] = (p[a] + 1).min(n[a] - 1);
                lo[a] = p[a].saturating_sub(1);
                0.5 * (f64::from(smoothed[cell_of(hi, n)]) - f64::from(smoothed[cell_of(lo, n)]))
            });
            let size = grad.iter().map(|g| g * g).sum::<f64>().sqrt();
            let normal = grad.map(|g| if size > 1e-12 { -g / size } else { 0.0 });
            let mut out = ChartEntry { cell: c, ..Default::default() };
            for a in 0..3 {
                let line = |t: usize| {
                    let mut q = p;
                    q[a] = t;
                    cell_of(q, n)
                };
                let starts: Vec<bool> =
                    (0..n[a]).map(|t| water[line(t)] > 0.5 && (t == 0 || water[line(t - 1)] <= 0.5)).collect();
                let runs_to = |t: usize| starts[..t].iter().filter(|&&s| s).count() as u32;
                let total = runs_to(n[a]);
                let before = runs_to(p[a]);
                let (sheet_plus, sheet_minus) = (cpu_sheet(before, sheets), cpu_sheet(total - before, sheets));
                let (mut in_plus, mut in_minus) = (0u32, 0u32);
                for t in (0..n[a]).filter(|&t| collar[line(t)] != 0) {
                    let runs = runs_to(t + 1);
                    in_plus += u32::from(cpu_sheet(runs, sheets) == sheet_plus);
                    in_minus += u32::from(cpu_sheet(total - runs, sheets) == sheet_minus);
                }
                out.view_plus[a] = (normal[a].max(0.0) / f64::from(in_plus.max(1)).sqrt()) as f32;
                out.view_minus[a] = ((-normal[a]).max(0.0) / f64::from(in_minus.max(1)).sqrt()) as f32;
                out.sheets |= (sheet_plus | (sheet_minus << 4)) << (8 * a);
            }
            out
        })
        .collect()
}

/// The chart plane slot of view v through cell p: (v · sheets + sheet) · M²
/// + the cell's place on the plane, lower of the other two axes first.
fn cpu_slot(p: [usize; 3], view: usize, sheet: usize, n: [usize; 3], sheets: usize) -> usize {
    let m = n.into_iter().max().unwrap();
    let a = view / 2;
    let (lower, upper) = match a {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    };
    (view * sheets + sheet) * m * m + p[lower] + m * p[upper]
}

fn view_share(entry: &ChartEntry, view: usize) -> f32 {
    if view.is_multiple_of(2) { entry.view_plus[view / 2] } else { entry.view_minus[view / 2] }
}

fn view_sheet(entry: &ChartEntry, view: usize) -> usize {
    ((entry.sheets >> (4 * view)) & 15) as usize
}

/// Run one atom from harness arrays into a fresh `out` of `len` elements.
fn run_into<P: Primitive, T: bytemuck::Pod + crate::node_graph::ports::KnownItem>(
    harness: &mut Harness,
    prim: &mut P,
    inputs: &[(&'static str, Slot)],
    len: usize,
    step_params: &ParamValues,
) -> Vec<T> {
    let out = harness.array::<T>(&[], len);
    let (_, errors) = harness.run(prim, inputs, &[("out", out.0)], step_params);
    assert!(errors.is_empty(), "{errors:?}");
    read(&out.1, len)
}

/// A collar problem: water, its collar flags, running total and entry list.
struct Collar {
    water: Vec<f32>,
    flags: Vec<u32>,
    total: Vec<u32>,
    count: usize,
}

impl Collar {
    fn new(seed: u64) -> Self {
        let water = random_water(LATTICE, seed);
        let flags = cpu_collar(&water, LATTICE);
        let total = cpu_running_total(&flags);
        let count = *total.last().unwrap() as usize;
        assert!(count > 20, "the test lattice needs a real collar, got {count}");
        Self { water, flags, total, count }
    }
}

#[test]
fn swash_collar_cells_and_select_flagged_match_cpu() {
    let mut harness = Harness::new();
    let collar = Collar::new(0xc011a);
    let cells = collar.water.len();
    let water = harness.array(&collar.water, cells);
    let flags: Vec<u32> = run_into(&mut harness, &mut CollarCells::new(), &[("water", water.0)], cells, &lattice_params(LATTICE, &[]));
    assert_eq!(flags, collar.flags, "collar flags");

    let total = harness.array(&collar.total, cells);
    // Room to spare (sentinels past the collar), and too little (the list is cut).
    for capacity in [collar.count + 7, collar.count / 2] {
        let got: Vec<u32> =
            run_into(&mut harness, &mut SelectFlagged::new(), &[("total", total.0)], capacity, &params(&[("capacity", capacity as f32)]));
        assert_eq!(got, cpu_entries(&collar.flags, capacity), "capacity {capacity}");
    }
}

#[test]
fn swash_chart_entries_match_cpu() {
    let mut harness = Harness::new();
    let collar = Collar::new(0xc4a7);
    let cells = collar.water.len();
    let smoothed = random_values(cells, 0x5300);
    let entries = cpu_entries(&collar.flags, collar.count + 5);
    let sheets = 3;
    let want = cpu_chart_entries(&entries, &collar.water, &smoothed, &collar.flags, LATTICE, sheets);
    let inputs = [
        ("entries", harness.array(&entries, entries.len()).0),
        ("water", harness.array(&collar.water, cells).0),
        ("smoothed", harness.array(&smoothed, cells).0),
        ("collar", harness.array(&collar.flags, cells).0),
    ];
    let got: Vec<ChartEntry> = run_into(
        &mut harness,
        &mut ChartEntries::new(),
        &inputs,
        entries.len(),
        &lattice_params(LATTICE, &[("sheets", sheets as f32)]),
    );
    let capped = want.iter().filter(|e| (0..6).any(|v| view_sheet(e, v) == sheets as usize - 1)).count();
    assert!(capped > 0, "no entry reaches the sheet cap");
    for (e, (g, w)) in got.iter().zip(&want).enumerate() {
        assert_eq!((g.cell, g.sheets), (w.cell, w.sheets), "entry {e}");
        for (a, b) in g.view_plus.iter().chain(&g.view_minus).zip(w.view_plus.iter().chain(&w.view_minus)) {
            assert!((a - b).abs() < 1e-5, "entry {e}: {g:?} vs {w:?}");
        }
    }
}

#[test]
fn swash_chart_sums_and_spread_match_cpu() {
    let mut harness = Harness::new();
    let collar = Collar::new(0x5055);
    let cells = collar.water.len();
    let n = LATTICE;
    let sheets = 3usize;
    let h = 0.25_f32;
    // Room for every entry, so each collar cell's sums are complete.
    let entries = cpu_entries(&collar.flags, collar.count + 3);
    let charts = cpu_chart_entries(&entries, &collar.water, &random_values(cells, 0x1e7), &collar.flags, n, sheets as u32);
    let k = entries.len();
    let value = random_values(k + 1, 0xfa1);
    let m = n.into_iter().max().unwrap();
    let planes_len = 6 * sheets * m * m;
    let lattice = lattice_params(n, &[("sheets", sheets as f32), ("cell_size", h)]);

    let mut want = vec![0.0f64; planes_len];
    for (e, entry) in charts.iter().enumerate().filter(|(_, c)| c.cell != u32::MAX) {
        let p = coords(entry.cell as usize, n);
        for view in 0..6 {
            want[cpu_slot(p, view, view_sheet(entry, view), n, sheets)] += f64::from(view_share(entry, view)) * f64::from(value[e]);
        }
    }
    let total = harness.array(&collar.total, cells);
    let charts_in = harness.array(&charts, k);
    let value_in = harness.array(&value, k + 1);
    let sums: Vec<f32> = run_into(
        &mut harness,
        &mut ChartSums::new(),
        &[("total", total.0), ("entries", charts_in.0), ("value", value_in.0)],
        planes_len,
        &lattice,
    );
    assert!(want.iter().filter(|v| **v != 0.0).count() > 20, "the sums test touches too few slots");
    assert_close(&sums, &want, "chart sums");

    let planes = random_values(planes_len, 0x91a);
    let mut want = vec![0.0f64; k + 1];
    for (e, entry) in charts.iter().enumerate().filter(|(_, c)| c.cell != u32::MAX) {
        let p = coords(entry.cell as usize, n);
        want[e] = 2.0 / f64::from(h) * f64::from(value[e])
            + (0..6)
                .map(|view| {
                    let sheet = view_sheet(entry, view).min(sheets - 1);
                    f64::from(view_share(entry, view)) * f64::from(planes[cpu_slot(p, view, sheet, n, sheets)])
                })
                .sum::<f64>();
    }
    want[k] = f64::from(value[k]);
    let planes_in = harness.array(&planes, planes_len);
    let spread: Vec<f32> = run_into(
        &mut harness,
        &mut ChartSpread::new(),
        &[("entries", charts_in.0), ("planes", planes_in.0), ("value", value_in.0)],
        k + 1,
        &lattice,
    );
    assert_close(&spread, &want, "chart spread");
}

#[test]
fn swash_collar_source_gather_and_pressure_match_cpu() {
    let mut harness = Harness::new();
    let collar = Collar::new(0x9a7e);
    let cells = collar.water.len();
    let total = harness.array(&collar.total, cells);

    // Source: entry values onto their cells; entries past the vector's are dropped.
    for k in [collar.count, collar.count / 2] {
        let value = random_values(k + 1, 0x50c + k as u64);
        let mut want = vec![0.0f64; cells];
        for (e, &c) in cpu_entries(&collar.flags, k).iter().enumerate().filter(|(_, c)| **c != u32::MAX) {
            want[c as usize] = f64::from(value[e]);
        }
        let value_in = harness.array(&value, k + 1);
        let grid: Vec<f32> =
            run_into(&mut harness, &mut CollarSource::new(), &[("total", total.0), ("value", value_in.0)], cells, &params(&[]));
        assert_close(&grid, &want, &format!("collar source, {k} entries"));
    }

    // Gather: grid at the entries minus c (vector[K], or 0 for a short vector), then sum / cells.
    let entries = cpu_entries(&collar.flags, collar.count + 4);
    let k = entries.len();
    let grid = random_values(cells, 0x6a1d);
    let sum = 3.5_f32;
    for vector in [random_values(k + 1, 0x7ec7), vec![9.0]] {
        let c = if vector.len() > k { f64::from(vector[k]) } else { 0.0 };
        let mut want: Vec<f64> = entries
            .iter()
            .map(|&cell| if cell == u32::MAX { 0.0 } else { f64::from(grid[cell as usize]) - c })
            .collect();
        want.push(f64::from(sum) / cells as f64);
        let inputs = [
            ("entries", harness.array(&entries, k).0),
            ("grid", harness.array(&grid, cells).0),
            ("vector", harness.array(&vector, vector.len()).0),
            ("sum", harness.array(&[sum], 1).0),
        ];
        let got: Vec<f32> = run_into(&mut harness, &mut CollarGather::new(), &inputs, k + 1, &params(&[]));
        assert_close(&got, &want, &format!("collar gather, vector of {}", vector.len()));
    }

    // Pressure: solved − correction + the vector's last element, in water only.
    let (solved, correction, vector) = (random_values(cells, 0x501), random_values(cells, 0xc0e), random_values(7, 0x1a57));
    let want: Vec<f64> = (0..cells)
        .map(|c| {
            if collar.water[c] > 0.5 {
                f64::from(solved[c]) - f64::from(correction[c]) + f64::from(vector[6])
            } else {
                0.0
            }
        })
        .collect();
    let inputs = [
        ("water", harness.array(&collar.water, cells).0),
        ("solved", harness.array(&solved, cells).0),
        ("correction", harness.array(&correction, cells).0),
        ("vector", harness.array(&vector, 7).0),
    ];
    let got: Vec<f32> = run_into(&mut harness, &mut CollarPressure::new(), &inputs, cells, &params(&[]));
    assert_close(&got, &want, "collar pressure");
}
