//! The step's params read from their wires and refused by name: never
//! clamped, so a value the step cannot run is a named error.
use super::super::gpu_flip_pressure::{MAX_ITERATIONS, Stop, level_refusal};

/// The Solve Level, refused unless it is a whole number the lattice's
/// levels reach (GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 11 (Solve Level)):
/// never clamped, so a level the lattice lacks is a named error.
pub(crate) fn read_solve_level(value: f32, lattice: [u32; 3]) -> Result<usize, String> {
    if !(value.is_finite() && value >= 0.0 && value.fract() == 0.0) {
        return Err(format!(
            "Coarse Solve must be a whole number from 0, not {value}"
        ));
    }
    let level = value as usize;
    match level_refusal(lattice, level) {
        Some(reason) => Err(reason),
        None => Ok(level),
    }
}

/// The Closed Faces mask, refused unless it is a whole number in 0..=63.
pub(crate) fn read_closed_faces(value: f32) -> Result<u32, String> {
    if value.fract() == 0.0 && (0.0..=63.0).contains(&value) {
        Ok(value as u32)
    } else {
        Err(format!(
            "Closed Faces must be a whole number from 0 to 63, not {value}"
        ))
    }
}

/// The sheet fill rate: the share of sheet seeds kept, 0 (off) to 1.
pub(crate) fn read_sheet_fill_rate(value: f32) -> Result<f32, String> {
    if !(value.is_finite() && (0.0..=1.0).contains(&value)) {
        return Err(format!("Sheet Fill Rate must be from 0 to 1, not {value}"));
    }
    Ok(value)
}

/// Max Iterations, the cap Auto converges within: refused unless finite and,
/// rounded, 1 to MAX_ITERATIONS.
pub(crate) fn read_max_iterations(value: f32) -> Result<u32, String> {
    let v = value.round();
    if value.is_finite() && (1.0..=MAX_ITERATIONS as f32).contains(&v) {
        Ok(v as u32)
    } else {
        Err(format!(
            "Max Iterations must be 1 to {MAX_ITERATIONS}, not {value}"
        ))
    }
}

/// When the solves stop: Auto at 0 or below converges within Max Iterations;
/// a positive value runs exactly that many, whatever Max Iterations says,
/// refused past MAX_ITERATIONS.
pub(super) fn read_iterations(value: f32, max_iterations: u32) -> Result<Stop, String> {
    if !value.is_finite() {
        return Err(format!("Iterations must be a number, not {value}"));
    }
    match value.round() {
        v if v <= 0.0 => Ok(Stop::Converged(max_iterations)),
        v if v > MAX_ITERATIONS as f32 => Err(format!(
            "Iterations {v} is past the solver's {MAX_ITERATIONS}"
        )),
        v => Ok(Stop::Fixed(v as u32)),
    }
}
