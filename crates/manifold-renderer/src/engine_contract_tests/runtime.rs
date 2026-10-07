#[path = "runtime_trigger_initialization.rs"]
mod trigger_initialization;

#[path = "runtime_bug080_manifest_gate_tests.rs"]
mod bug080_manifest_gate_tests;

#[path = "runtime_segment_prewarm_tests.rs"]
mod segment_prewarm_tests;

#[cfg(feature = "gpu-proofs")]
#[path = "runtime_group_mask_tests.rs"]
mod group_mask_tests;

#[cfg(feature = "gpu-proofs")]
#[path = "runtime_multi_segment_tests.rs"]
mod multi_segment_tests;

#[cfg(feature = "gpu-proofs")]
#[path = "runtime_binding_seed_tests.rs"]
mod binding_seed_tests;

#[cfg(feature = "gpu-proofs")]
#[path = "runtime_user_binding_tests.rs"]
mod user_binding_tests;

#[cfg(feature = "gpu-proofs")]
#[path = "runtime_generator_input_tests.rs"]
mod generator_input_tests;

#[cfg(feature = "gpu-proofs")]
#[path = "runtime_chain_error_tests.rs"]
mod chain_error_tests;

#[cfg(feature = "gpu-proofs")]
#[path = "runtime_amount_zero_passthrough_tests.rs"]
mod amount_zero_passthrough_tests;

#[cfg(feature = "gpu-proofs")]
#[path = "runtime_blob_grain_probe_tests.rs"]
mod blob_grain_probe_tests;

#[cfg(feature = "gpu-proofs")]
#[path = "runtime_mosh_tests.rs"]
mod mosh_tests;

#[cfg(feature = "gpu-proofs")]
#[path = "runtime_topology_hash_tests.rs"]
mod topology_hash_tests;

#[cfg(feature = "gpu-proofs")]
#[path = "runtime_transient_slot_tests.rs"]
mod transient_slot_tests;
