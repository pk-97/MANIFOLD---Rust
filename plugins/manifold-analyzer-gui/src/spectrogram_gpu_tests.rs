//! Bounded native Metal pixel proofs for the shipped spectrogram shader.
use super::*;
use crate::spectrum_worker::spectrogram_tests::column_message;

fn read_pixels(device: &GpuDevice, renderer: &SpectrumGpuRenderer) -> Vec<u8> {
    let bytes = renderer.width() as u64 * renderer.height() as u64 * 4;
    let readback = device.create_buffer_shared(bytes);
    let mut encoder = device.create_encoder("spectrogram proof readback");
    encoder.copy_texture_to_buffer(
        renderer.target.gpu_texture(),
        &readback,
        renderer.width(),
        renderer.height(),
        renderer.width() * 4,
    );
    encoder.commit_and_wait_completed();
    unsafe { std::slice::from_raw_parts(readback.mapped_ptr().unwrap(), bytes as usize) }.to_vec()
}
fn rgb(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 3] {
    let i = ((y * width + x) * 4) as usize;
    [pixels[i + 2], pixels[i + 1], pixels[i]]
}
fn expected_rgb(db: f32) -> [u8; 3] {
    let stops = [
        (0.0, [0.0, 0.0, 0.0]),
        (0.15, [0.0, 0.0, 0.45]),
        (0.35, [0.0, 0.10, 0.95]),
        (0.55, [0.0, 0.80, 0.95]),
        (0.70, [0.20, 0.95, 0.20]),
        (0.80, [0.95, 0.95, 0.0]),
        (0.90, [0.95, 0.0, 0.0]),
        (1.0, [1.0, 1.0, 1.0]),
    ];
    let t = ((db + 60.0) / 60.0).clamp(0.0, 1.0).powf(0.7);
    let pair = stops.windows(2).find(|p| t <= p[1].0).unwrap();
    let f = (t - pair[0].0) / (pair[1].0 - pair[0].0);
    std::array::from_fn(|i| ((pair[0].1[i] * (1.0 - f) + pair[1].1[i] * f) * 255.0).round() as u8)
}
fn save(name: &str, pixels: &[u8], width: u32, height: u32) {
    // Optional artifact destination; always execute the assertions.
    if let Ok(dir) = std::env::var("MANIFOLD_SPECTROGRAM_PROOF_DIR") {
        std::fs::create_dir_all(&dir).unwrap();
        let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
        for pixel in pixels.chunks_exact(4) {
            ppm.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
        }
        std::fs::write(std::path::Path::new(&dir).join(format!("{name}.ppm")), ppm).unwrap();
    }
}
fn display(stacked: bool, sync: bool) -> DisplayConfig {
    DisplayConfig {
        spectrum_fraction: 0.1,
        spectrogram_db_min: -60.0,
        spectrogram_db_max: 0.0,
        spectrogram_gamma: 0.7,
        stacked_mode: stacked,
        sync_mode: sync,
        ..Default::default()
    }
}

#[test]
fn metal_spectrogram_colours_readouts_and_narrow_bands() {
    let device = GpuDevice::new();
    let params = cqt_build_params(48000.0);
    let bins = (params.bpo as f32 * (params.fmax / params.fmin).log2()).floor() as usize;
    let width = 256;
    let height = 128;
    let mut renderer = SpectrumGpuRenderer::new(&device, width, height, width, bins).unwrap();
    renderer.set_history_weighting(crate::Weighting::Flat, 0.0);
    for col in 0..HISTORY_COLS {
        let mut data = vec![-140.0; bins];
        let mut right = data.clone();
        // Includes single-bin lines narrower than a stacked display pixel.
        for k in (8..bins - 8).step_by(11) {
            data[k] = -48.0 + (col % 13) as f32 * 3.0;
            right[k] = -24.0;
        }
        renderer.apply_column(&column_message(col, data, Some(right)));
    }
    for stacked in [false, true] {
        for sync in [false, true] {
            renderer.set_display(display(stacked, sync));
            renderer.render(
                &device,
                &vec![-120.0; width as usize],
                &vec![-120.0; width as usize],
                20.0,
                20000.0,
                -60.0,
                0.0,
            );
            let pixels = read_pixels(&device, &renderer);
            let top = (height as f32 * 0.1).round();
            let spec_height = height as f32 - top;
            for y in (top as u32 + 1)..height - 1 {
                for x in (3..width - 2).step_by(17) {
                    let py = y as f32 + 0.5 - top;
                    let sub_h = spec_height / if stacked { 2.0 } else { 1.0 };
                    let buffer = if stacked && py >= sub_h { 1 } else { 0 };
                    let sub_y = py - buffer as f32 * sub_h;
                    let freq = 20000.0_f32 * (20.0_f32 / 20000.0).powf(sub_y / sub_h);
                    let bin = params.bpo as f32 * (freq / params.fmin).log2();
                    let span = params.bpo as f32 * (20000.0_f32 / 20.0).log2() / sub_h;
                    let col = if sync {
                        (x as f32 + 0.5) / width as f32 * HISTORY_COLS as f32
                    } else {
                        (HISTORY_COLS - 1 - (width - 1 - x)) as f32
                    };
                    let db = renderer.sample_history_db(buffer, col, bin, span).unwrap();
                    let expected = expected_rgb(db);
                    let actual = rgb(&pixels, width, x, y);
                    for c in 0..3 {
                        assert!((actual[c] as i16-expected[c] as i16).abs()<=2,
                "pixel/readout mismatch stacked={stacked} sync={sync} ({x},{y}): {actual:?} vs {expected:?}, {db}");
                    }
                }
            }
            save(
                &format!("bands-stacked-{stacked}-sync-{sync}"),
                &pixels,
                width,
                height,
            );
        }
    }
    // Exact colour stops / floor / overload at the same actual shader target.
    for db in [-140.0, -60.0, -50.0, -40.0, -30.0, -20.0, -10.0, 0.0, 6.0] {
        renderer.apply_column(&column_message(HISTORY_COLS - 1, vec![db; bins], None));
        renderer.set_display(display(false, false));
        renderer.render(
            &device,
            &vec![-120.0; width as usize],
            &vec![-120.0; width as usize],
            20.0,
            20000.0,
            -60.0,
            0.0,
        );
        let pixels = read_pixels(&device, &renderer);
        assert_eq!(
            rgb(&pixels, width, width - 1, height / 2),
            expected_rgb(db),
            "colour at {db}dB"
        );
    }
    renderer.set_history_weighting(crate::Weighting::Pink, 0.0);
    let frequency = CQT_FMIN_HZ * (240.0 / CQT_BINS_PER_OCTAVE as f32).exp2();
    let db = renderer
        .sample_history_db(0, (HISTORY_COLS - 1) as f32, 240.0, 1.0)
        .unwrap();
    assert!(
        (db - (6.0 + crate::weighting_db_at(crate::Weighting::Pink, frequency))).abs() < 0.001,
        "retained history did not adopt new weighting"
    );
    renderer.set_history_weighting(crate::Weighting::Flat, 0.0);
    assert!(
        (renderer
            .sample_history_db(0, (HISTORY_COLS - 1) as f32, 240.0, 1.0)
            .unwrap()
            - 6.0)
            .abs()
            < 0.001
    );
}

#[test]
fn metal_spectrogram_audio_fixture_images() {
    use crate::spectrum_worker::spectrogram_tests::{columns, noise};
    let device = GpuDevice::new();
    let rate = 48000.0;
    for kind in ["white", "pink", "tones", "bursts", "silence"] {
        let audio = match kind {
            "white" => noise(rate, false),
            "pink" => noise(rate, true),
            _ => (0..131072)
                .map(|i| {
                    let t = i as f64 / rate as f64;
                    match kind {
                        "tones" => [1000.0, 1400.0, 12000.0, 18000.0]
                            .iter()
                            .map(|f| (0.035 * (std::f64::consts::TAU * f * t).sin()) as f32)
                            .sum(),
                        "bursts" => {
                            if i % 12000 < 64 {
                                (0.25 * (std::f64::consts::TAU * 18000.0 * t).sin()) as f32
                            } else {
                                0.0
                            }
                        }
                        _ => 0.0,
                    }
                })
                .collect(),
        };
        for sharpen in [false, true] {
            let (freqs, cols) = columns(&audio[..16_384], rate, sharpen);
            let width = 64;
            let height = 512;
            let mut renderer =
                SpectrumGpuRenderer::new(&device, width, height, width, freqs.len()).unwrap();
            for (col, mut data) in cols.into_iter().enumerate() {
                if kind == "pink" {
                    crate::tilt_cqt_column(
                        &mut data,
                        crate::Weighting::Pink,
                        crate::weighting_align_offset(crate::Weighting::Pink, 20.0, 20000.0),
                        CQT_FMIN_HZ,
                        1.0 / CQT_BINS_PER_OCTAVE as f32,
                    );
                }
                renderer.apply_column(&column_message(col as u32, data, None));
            }
            renderer.set_display(display(false, false));
            renderer.render(
                &device,
                &vec![-120.0; width as usize],
                &vec![-120.0; width as usize],
                20.0,
                20000.0,
                -60.0,
                0.0,
            );
            let pixels = read_pixels(&device, &renderer);
            if kind == "silence" {
                assert!(pixels[(width * 52 * 4) as usize..]
                    .chunks_exact(4)
                    .all(|p| p[..3] == [0, 0, 0]));
            }
            save(&format!("{kind}-sharpen-{sharpen}"), &pixels, width, height);
        }
    }
}
