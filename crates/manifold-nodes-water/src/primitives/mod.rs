// The liquid-surface proofs name FLIP and Matter, so they link from here (D8).
#[cfg(all(test, feature = "gpu-proofs"))]
mod liquid_surface_tests;
#[cfg(test)]
mod face_grid_extent_tests;
// The whitewater extent proof sizes against Matter and the particle volume,
// so it links from here (D8).
#[cfg(test)]
mod whitewater_extent_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod face_grid_tests;

// The brick-consumer validation includes FLIP's clamp, so it links from here (D8).
#[cfg(test)]
mod liquid_bricks_consumer_tests;
// The counting-sort proofs cover matter records, so they link from here (D8).
#[cfg(all(test, feature = "gpu-proofs"))]
mod sort_particles_into_cells {
    mod gpu_tests;
}
// The pose agreement proof names FLIP and Matter, so it links from here (D8).
#[cfg(test)]
mod liquid_solid_distance {
    mod tests;
}
