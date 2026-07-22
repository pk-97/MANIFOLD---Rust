//! [`PresetRuntime`] — one cached [`Graph`] per `EffectChain`.
//!
//! Each chain compiles its full effect sequence (every active
//! [`PresetInstance`], plus `Mix` sub-graphs for wet/dry groups)
//! into a single graph runtime instance: one [`Graph`], one
//! [`ExecutionPlan`], one [`MetalBackend`], one [`Executor`]. That's
//! one ping/pong recycle pool for the chain, one executor step loop
//! per frame, one input-texture pre-bind per frame — no per-effect
//! dispatch overhead.
//!
//! Primitive state (mip pyramids, feedback buffers, depth workers)
//! lives inside the boxed [`EffectNode`] owned by the cached
//! [`Graph`]. Per-frame param changes refresh in place via
//! [`apply_bindings`]; topology changes (effect added /
//! removed / reordered / type-swapped, group enabled / disabled
//! toggle, group crossing the 1.0 wet/dry boundary, render-resolution
//! change) rebuild from scratch.
//!
//! ## Build-time wiring
//!
//! Linear sequence:
//!
//! ```text
//! Source ──▶ eff_1 ──▶ eff_2 ──▶ … ──▶ eff_n ──▶ FinalOutput
//! ```
//!
//! Wet/dry group with `wet_dry < 1.0` (spans effects `e_i..e_j`):
//!
//! ```text
//! pre_group ─┬─▶ e_i ──▶ … ──▶ e_j ──▶ Mix.b
//!            └────────────────────────▶ Mix.a
//! Mix.out (= lerp(dry, wet, wet_dry)) ─▶ next_node
//! ```
//!
//! ## Per-frame cost
//!
//! - 1 `copy_texture_to_texture` (upstream input → source slot)
//! - 1 `apply_bindings` call per effect (unified static + user tail)
//! - K `set_param` calls (one per Mix node, refreshing `amount`)
//! - 1 `execute_frame_with_gpu` covering N + K + 2 step iterations
//!   (Source + N effects + K Mix nodes + FinalOutput)
//! - 1 `texture_2d` lookup for the chain output
//!
//! The single `copy_texture_to_texture` is the only residual overhead
//! relative to the legacy chain's direct-from-input first-effect
//! dispatch: the backend's slot API takes owned `RenderTarget`s, not
//! borrowed `&GpuTexture`s, so the upstream input is materialised
//! into the source slot once per chain invocation.

use ahash::AHashMap;
use manifold_core::PresetTypeId;
use manifold_core::NodeId;
use manifold_core::effects::{EffectGroup, PresetInstance, RelightField, RelightParams};
use manifold_core::id::{EffectGroupId, EffectId};
use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureFormat, TexturePool};

use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::primitives::Mix;
use crate::node_graph::{
    BindingSource, BoundGraph, EffectGraphDefExt, ExecutionPlan, Executor, FINAL_OUTPUT_TYPE_ID,
    FinalOutput, FrameTime, GENERATOR_INPUT_TYPE_ID, Graph, GraphError, LoadError, LoadedPresetView,
    MetalBackend, NodeInstanceId, ParamBinding, ParamValue, PrimitiveRegistry, ResolvedBinding,
    ResolvedTarget,
    ResourceId, Slot, Source, SpliceResult, StateStore, apply_binding_defaults, compile,
    splice_def_into_chain,
};
use crate::node_graph::{is_skipped_for, loaded_preset_view_by_id};
use crate::preset_context::PresetContext;
use manifold_core::effect_graph_def::{EFFECT_GRAPH_VERSION_WITH_METADATA, EffectGraphDef};
use manifold_core::params::ParamManifest;
use manifold_core::{Beats, Seconds};
use crate::render_target::RenderTarget;

mod errors;
pub use errors::{ChainError, JsonGeneratorLoadError};
use errors::record_chain_error;

mod bindings;
use bindings::{StringBindingResolution, def_string_param_value, RelightParamWrite, build_relight_writes};

mod segments;
pub use segments::{prewarm_chain_segments, prewarm_project_chain_segments};
use segments::{SegmentMember, classify_segment_member, segment_run, build_segment_cards};

mod build;
use build::{compute_topology_hash, close_mix_group, assign_texture2d_slots, OpenGroup};

mod core;
pub use core::PresetRuntime;
use core::{EffectSlot, PresetIo, chain_active_effects};
#[cfg(test)]
use core::assert_manifest_gate;
#[cfg(all(test, feature = "gpu-proofs"))]
use core::GRAPH_FORMAT;

mod instrumentation;

#[cfg(all(test, feature = "gpu-proofs"))]
#[path = "tests/multi_segment.rs"]
mod multi_segment_tests;

#[cfg(all(test, feature = "gpu-proofs"))]
#[path = "tests/binding_seed.rs"]
mod binding_seed_tests;

#[cfg(test)]
#[path = "tests/topology_hash.rs"]
mod topology_hash_tests;

#[cfg(all(test, feature = "gpu-proofs"))]
#[path = "tests/user_binding.rs"]
mod user_binding_tests;

#[cfg(test)]
#[path = "tests/bug080_manifest_gate.rs"]
mod bug080_manifest_gate_tests;

#[cfg(test)]
#[path = "tests/persistent_slot.rs"]
mod persistent_slot_tests;

#[cfg(all(test, feature = "gpu-proofs"))]
#[path = "tests/generator_input.rs"]
mod generator_input_tests;

#[cfg(all(test, feature = "gpu-proofs"))]
#[path = "tests/chain_error.rs"]
mod chain_error_tests;

#[cfg(test)]
#[path = "tests/generator_runtime.rs"]
mod generator_runtime_tests;

#[cfg(all(test, feature = "gpu-proofs"))]
#[path = "tests/chain_fusion.rs"]
mod chain_fusion_tests;

#[cfg(test)]
#[path = "tests/segment_prewarm.rs"]
mod segment_prewarm_tests;
