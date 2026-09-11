//! Standalone CPU water-cache exporter.
//!
//! Compile directly with `rustc`; this file intentionally lives below
//! `tests/support` so Cargo does not discover it as an integration test.

#[path = "../water_apic_coupled_reference.rs"]
mod coupled;

use std::path::Path;

fn main() {
    let mut args = std::env::args().skip(1);
    let output_dir = match args.next() {
        Some(value) => value,
        None => usage_and_exit(),
    };
    let frame_count = match args.next().and_then(|value| value.parse::<usize>().ok()) {
        Some(value) => value,
        None => usage_and_exit(),
    };
    let fps = match args.next().and_then(|value| value.parse::<f64>().ok()) {
        Some(value) => value,
        None => usage_and_exit(),
    };
    if args.next().is_some() {
        usage_and_exit();
    }
    if let Err(error) = coupled::export_offline_cache(Path::new(&output_dir), frame_count, fps) {
        eprintln!("water offline export failed: {error}");
        std::process::exit(1);
    }
}

fn usage_and_exit() -> ! {
    eprintln!("usage: water_offline_export OUTPUT_DIR FRAME_COUNT FPS");
    std::process::exit(2);
}
