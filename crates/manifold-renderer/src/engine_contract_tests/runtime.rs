#[path = "runtime_trigger_initialization.rs"]
mod trigger_initialization;

#[path = "runtime_bug080_manifest_gate_tests.rs"]
mod bug080_manifest_gate_tests;

#[path = "runtime_segment_prewarm_tests.rs"]
mod segment_prewarm_tests;

#[cfg(feature = "gpu-proofs")]
#[path = "runtime_group_mask_tests.rs"]
mod group_mask_tests;
