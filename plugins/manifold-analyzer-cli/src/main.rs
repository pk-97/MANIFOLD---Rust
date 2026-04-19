//! Headless spectrum analysis CLI.
//!
//! Loads a WAV file, runs it through the same `Analyzer` the VST3 plugin uses,
//! and prints the time-averaged magnitude spectrum as CSV to stdout. Lets us
//! validate DSP correctness without opening a DAW.
//!
//! Usage:
//!   manifold-analyzer-cli <path/to/audio.wav> [fft_size]
//!
//! Output: `bin,freq_hz,avg_db` per line.

use manifold_analyzer_dsp::{Analyzer, MIN_DB};
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: manifold-analyzer-cli <path/to/audio.wav> [fft_size]");
        return ExitCode::from(2);
    };
    let fft_size: usize = args
        .next()
        .map(|s| s.parse().expect("fft_size must be a positive integer"))
        .unwrap_or(4096);

    match run(PathBuf::from(path), fft_size) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(path: PathBuf, fft_size: usize) -> Result<(), String> {
    let mut reader = hound::WavReader::open(&path).map_err(|e| format!("open {path:?}: {e}"))?;
    let spec = reader.spec();
    let sample_rate = spec.sample_rate as f32;
    let channels = spec.channels as usize;

    eprintln!(
        "loaded {path:?} — {} Hz, {} ch, {} samples, fft_size={fft_size}",
        spec.sample_rate,
        channels,
        reader.duration(),
    );

    let mono = read_mono_f32(&mut reader, channels).map_err(|e| format!("decode: {e}"))?;

    let mut analyzer = Analyzer::new(sample_rate, fft_size);
    let mut sum = vec![0.0f64; analyzer.num_bins()];
    let mut count = 0usize;

    analyzer.process_mono(&mono, |spectrum| {
        for (i, &db) in spectrum.iter().enumerate() {
            sum[i] += db as f64;
        }
        count += 1;
    });

    if count == 0 {
        return Err(format!(
            "input too short ({} samples) for fft_size {fft_size}",
            mono.len()
        ));
    }

    println!("bin,freq_hz,avg_db");
    for (bin, total) in sum.iter().enumerate() {
        let avg = (total / count as f64) as f32;
        let avg = avg.max(MIN_DB);
        println!("{bin},{:.2},{:.2}", analyzer.bin_frequency(bin), avg);
    }

    eprintln!("analyzed {count} frames");
    Ok(())
}

fn read_mono_f32<R: std::io::Read>(
    reader: &mut hound::WavReader<R>,
    channels: usize,
) -> Result<Vec<f32>, hound::Error> {
    let spec = reader.spec();
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let max = ((1u64 << (spec.bits_per_sample - 1)) - 1) as f32;
            reader
                .samples::<i32>()
                .map(|r| r.map(|s| s as f32 / max))
                .collect::<Result<_, _>>()?
        }
    };

    if channels <= 1 {
        return Ok(raw);
    }

    let mut mono = Vec::with_capacity(raw.len() / channels);
    for frame in raw.chunks_exact(channels) {
        let sum: f32 = frame.iter().sum();
        mono.push(sum / channels as f32);
    }
    Ok(mono)
}
