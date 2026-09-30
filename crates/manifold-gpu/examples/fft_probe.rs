//! Compute FFT against MPSGraph on SWASH's transforms (BUG-l2h3.7, own FFT
//! kernels): the 3D real pair on [n, n, n] and the batched 2D pair on
//! [24, n, n]. Checks the two agree on the same data, then times them
//! interleaved, `BATCH` transforms per command buffer, and reports the median
//! GPU µs per transform and the CPU µs to encode one.
//!
//! `cargo run --release -p manifold-gpu --example fft_probe`

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    use manifold_gpu::compute_fft::{ComputeFft, ComputeFftKind};
    use manifold_gpu::{FftKind, GpuBuffer, GpuDevice, GpuFft};
    use std::time::Instant;

    const BATCH: usize = 20;
    const ROUNDS: usize = 15;

    fn upload(device: &GpuDevice, values: &[f32]) -> GpuBuffer {
        let buf = device.create_buffer_shared((values.len() * 4) as u64);
        let ptr = buf.mapped_ptr().expect("shared buffer is mapped");
        // SAFETY: shared buffer sized for `values`; no GPU work in flight.
        unsafe { std::slice::from_raw_parts_mut(ptr.cast::<f32>(), values.len()).copy_from_slice(values) };
        buf
    }
    fn download(buf: &GpuBuffer) -> Vec<f32> {
        let ptr = buf.mapped_ptr().expect("shared buffer is mapped");
        // SAFETY: shared buffer; GPU work done.
        unsafe { std::slice::from_raw_parts(ptr.cast::<f32>(), (buf.size / 4) as usize) }.to_vec()
    }
    fn median(mut v: Vec<f64>) -> f64 {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    }
    fn max_rel(a: &[f32], b: &[f32]) -> f64 {
        let peak = b.iter().map(|x| f64::from(x.abs())).fold(0.0, f64::max);
        a.iter().zip(b).map(|(x, y)| f64::from((x - y).abs())).fold(0.0, f64::max) / peak
    }

    let device = GpuDevice::new();
    // Set to a directory to keep the generated MSL for inspection.
    if let Ok(dir) = std::env::var("FFT_PROBE_MSL_DIR") {
        device.load_msl_cache(std::path::Path::new(&dir));
    }
    println!("case                         ours_gpu_us  mps_gpu_us  ratio  ours_enc_us  mps_enc_us  max_rel_diff");
    let mut per_frame = Vec::new();
    for n in [64u32, 96, 128] {
        for (axes, shape) in [(3u32, [n, n, n]), (2, [24, n, n])] {
            let dims = shape.map(|v| v as usize);
            let axes_list: Vec<usize> = if axes == 3 { vec![0, 1, 2] } else { vec![1, 2] };
            let count = dims.iter().product::<usize>();
            let mut state = n + axes;
            let values: Vec<f32> = (0..count)
                .map(|_| {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (state >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
                })
                .collect();
            let real = upload(&device, &values);
            for (kind, mps_kind, name) in [
                (ComputeFftKind::RealToHermitean, FftKind::RealToHermitean, "r2c"),
                (ComputeFftKind::HermiteanToReal, FftKind::HermiteanToReal, "c2r"),
            ] {
                let ours = ComputeFft::new(&device, kind, shape, axes).expect("plan");
                let mps = GpuFft::new_nd(&device, mps_kind, &dims, &axes_list);
                // The inverse reads a genuine half spectrum.
                let input = if kind == ComputeFftKind::RealToHermitean {
                    real.clone()
                } else {
                    let spectrum = device.create_buffer_shared(ours.spectrum_bytes());
                    let fwd = ComputeFft::new(&device, ComputeFftKind::RealToHermitean, shape, axes).expect("plan");
                    let mut enc = device.create_encoder("fft-probe-spectrum");
                    fwd.encode(&mut enc, &real, &spectrum);
                    enc.commit_and_wait_completed();
                    spectrum
                };
                let out_bytes = if kind == ComputeFftKind::RealToHermitean { ours.spectrum_bytes() } else { ours.real_bytes() };
                assert_eq!(out_bytes, mps.output_len_bytes(), "output sizes differ");
                let out_ours = device.create_buffer_shared(out_bytes);
                let out_mps = device.create_buffer_shared(out_bytes);
                let mut enc = device.create_encoder("fft-probe-check");
                ours.encode(&mut enc, &input, &out_ours);
                mps.encode(&mut enc, &input, &out_mps);
                enc.commit_and_wait_completed();
                let diff = max_rel(&download(&out_ours), &download(&out_mps));

                let (mut gpu_ours, mut gpu_mps, mut enc_ours, mut enc_mps) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
                for round in 0..ROUNDS + 2 {
                    for ours_first in [round % 2 == 0, round % 2 != 0] {
                        let mut enc = device.create_encoder("fft-probe-time");
                        let start = Instant::now();
                        for _ in 0..BATCH {
                            if ours_first {
                                ours.encode(&mut enc, &input, &out_ours);
                                enc.compute_memory_barrier_buffers();
                            } else {
                                mps.encode(&mut enc, &input, &out_mps);
                            }
                        }
                        let cpu = start.elapsed().as_secs_f64() * 1e6 / BATCH as f64;
                        let gpu = enc.commit_and_wait_completed_timed() * 1e6 / BATCH as f64;
                        // The first two rounds warm pipelines and caches.
                        if round >= 2 {
                            if ours_first {
                                gpu_ours.push(gpu);
                                enc_ours.push(cpu);
                            } else {
                                gpu_mps.push(gpu);
                                enc_mps.push(cpu);
                            }
                        }
                    }
                }
                let mut passes = Vec::new();
                for (index, (label, groups)) in ours.pass_shapes().into_iter().enumerate() {
                    let mut times = Vec::new();
                    for _ in 0..ROUNDS {
                        let mut enc = device.create_encoder("fft-probe-pass");
                        for _ in 0..BATCH {
                            ours.encode_pass(&mut enc, &input, &out_ours, index);
                            enc.compute_memory_barrier_buffers();
                        }
                        times.push(enc.commit_and_wait_completed_timed() * 1e6 / BATCH as f64);
                    }
                    passes.push(format!("{} {groups:?} {:.1}", label.trim_start_matches("compute_fft "), median(times)));
                }
                let (go, gm) = (median(gpu_ours), median(gpu_mps));
                println!(
                    "{:<28} {go:>11.1} {gm:>11.1} {:>6.2} {:>12.1} {:>11.1} {diff:>13.2e}",
                    format!("{name} axes{axes} {shape:?}"),
                    go / gm,
                    median(enc_ours),
                    median(enc_mps),
                );
                println!("    passes: {}", passes.join(" | "));
                per_frame.push((n, axes, name, go, gm));
            }
        }
    }

    // SWASH's default frame: 2 steps of 24 passes plus an 8-pass density
    // solve. A P-pass solve makes 2 + P forward and 2 + P inverse 3D calls,
    // and 1 + P of each batched 2D.
    let passes = [24u32, 24, 8];
    let calls_3d: u32 = passes.iter().map(|p| 2 + p).sum();
    let calls_2d: u32 = passes.iter().map(|p| 1 + p).sum();
    println!("\nper frame ({calls_3d} of each 3D direction, {calls_2d} of each batched 2D direction):");
    for n in [64u32, 96, 128] {
        let (mut ours, mut mps) = (0.0, 0.0);
        for &(m, axes, _, go, gm) in &per_frame {
            if m == n {
                let calls = f64::from(if axes == 3 { calls_3d } else { calls_2d });
                ours += go * calls;
                mps += gm * calls;
            }
        }
        println!("  n={n}: ours {:.2} ms, MPSGraph {:.2} ms", ours / 1000.0, mps / 1000.0);
    }
}
