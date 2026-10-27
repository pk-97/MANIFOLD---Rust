#[cfg(all(test, feature = "gpu-proofs"))]
mod running_total_tests;
// The whitewater extent proof sizes against Matter and the particle volume,
// so it links from here (D8).
#[cfg(test)]
mod whitewater_extent_tests;

// The counting-sort proofs cover matter records, so they link from here (D8).
#[cfg(all(test, feature = "gpu-proofs"))]
mod sort_particles_into_cells {
    mod gpu_tests;
}
