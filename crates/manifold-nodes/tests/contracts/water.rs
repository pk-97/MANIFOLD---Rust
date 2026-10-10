pub(crate) mod primitives;

#[cfg(feature = "water-race-probes")]
mod race_probe;

#[cfg(feature = "gpu-proofs")]
mod physics_scene;
mod coupling;
