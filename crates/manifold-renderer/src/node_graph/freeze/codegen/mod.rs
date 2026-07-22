//! Fusion codegen — emit WGSL kernels from atom `wgsl_body` fragments
//! (design doc §12). This module is the v1 foundation: the **standalone**
//! single-atom kernel generator. It wraps one atom's body fragment in the
//! iteration boilerplate (dims/guard/sample/store) + a merged param uniform,
//! reproducing that atom's hand-written kernel so the hand shader can be
//! deleted (single-source authoring; validated per-atom against the original
//! through the [`TextureDiff`](super::TextureDiff) oracle in build step 1b).
//!
//! The fused MULTI-atom generator (chaining N bodies, namespace+dedup) is
//! build step 3/4; it reuses this module's param-emission + read-path helpers.
//!
//! Determinism (design §12.3): output is byte-identical run-to-run — fields
//! emit in `PARAMS` slice order, the body is verbatim, and there are no
//! float-literal-from-param emissions (all params are live uniform reads in
//! v1, never baked constants). The generated WGSL text is the cross-session
//! pipeline-cache key, so determinism is load-bearing.

mod types;
mod uniforms;
mod entry_points;
mod standalone;
mod fused;
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests;
#[cfg(test)]
mod dispatch_contract_tests;

pub use fused::generate_fused;
pub(crate) use fused::rename_ident;

pub use standalone::{generate_standalone, StandaloneKernelSpec};

pub use entry_points::{standalone_for_node, standalone_for_spec, standalone_for_spec_fmt, wgsl_storage_token};

pub use types::{CodegenError, ENTRY, FusedVirtualChain, FusionRegion, GeneratedFusion, InputSource, RegionNode, VOLUME_WORKGROUP_3D};
pub(crate) use types::{param_is_fusable, param_wgsl_type, wgsl_safe_field};

