//! Source emission: the half-cell sites and the emit pass.

/// Half-cell sites sources emit at: eight a cell (gpu_flip_step/emit.wgsl
/// emit_sites).
pub(crate) fn emit_sites(cells: [u32; 3]) -> u64 {
    cells.iter().map(|&n| 2 * u64::from(n)).product()
}
