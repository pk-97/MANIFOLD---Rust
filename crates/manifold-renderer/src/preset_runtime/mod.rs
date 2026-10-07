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
//! toggle, group crossing the 1.0 wet/dry boundary) rebuild from scratch.
//! Resolution changes replace resources transactionally and reset simulation state.
//!
//! Each chain owns one compiled graph, execution plan, Metal backend, and
//! executor. Primitive state stays in the graph's boxed nodes; live parameter
//! changes apply in place, while topology or resolution changes rebuild it.

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
use crate::node_graph::loaded_preset_view_by_id;
use crate::preset_context::PresetContext;
use manifold_core::effect_graph_def::{EFFECT_GRAPH_VERSION_WITH_SCENE_MODIFIERS, EffectGraphDef};
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
pub use crate::node_graph::freeze::install::prewarm_worker_pending_count;
use segments::{SegmentMember, classify_segment_member, segment_run, build_segment_cards};

mod build;
mod device;
pub use build::chain_topology_hash;
use build::{assign_texture2d_slots, compute_topology_hash};

mod groups;
use groups::{chain_active_effects, close_mix_group, validate_mask_groups, OpenGroup};
mod physics_sampling;
mod physics_impulses;
mod scene_impulses;
pub use scene_impulses::SceneImpulseDiagnostics;
pub use physics_impulses::{CapturedSceneImpulse, PreparedSceneImpulse};
mod physics_carry;
mod physics_sources;
mod physics_source_runtime;
mod physics_source_controls;
mod physics_source_state;
mod physics_source_chain;
#[cfg(test)]
mod physics_source_state_tests;
mod convert_heal;
mod math_view;
mod math_view_events;
mod lifecycle;

mod core;
pub use core::{ChainBuildInputs, FrameContextInputs, PresetRuntime};
mod resize;
pub use resize::PreparedRuntimeResize;
mod debug;
pub use debug::{ChainDebugInfo, StepDebugInfo};
use core::{EffectSlot, PresetIo};
#[cfg(test)]
use core::assert_manifest_gate;
#[cfg(all(test, feature = "gpu-proofs"))]
use core::GRAPH_FORMAT;

mod dump_sets;
pub mod instrumentation;
pub(crate) mod beat_envelope;
mod scene_viewport;
mod modifier_preview;
mod modifier_runtime;
mod gpu_flip_surface;
pub use modifier_preview::{ModifierPreviewContext, ModifierPreviewError};

#[cfg(all(test, feature = "gpu-proofs"))]
#[path = "tests/multi_segment.rs"]
mod multi_segment_tests;

#[cfg(all(test, feature = "gpu-proofs"))]
#[path = "tests/group_mask.rs"]
mod group_mask_tests;

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

#[cfg(test)]
#[path = "tests/transient_slot.rs"]
mod transient_slot_tests;

#[cfg(all(test, feature = "gpu-proofs"))]
#[path = "tests/generator_input.rs"]
mod generator_input_tests;

#[cfg(all(test, feature = "gpu-proofs"))]
#[path = "tests/chain_error.rs"]
mod chain_error_tests;

#[cfg(all(test, feature = "gpu-proofs"))]
#[path = "tests/amount_zero_passthrough.rs"]
mod amount_zero_passthrough_tests;

#[cfg(test)]
#[path = "tests/generator_runtime.rs"]
mod generator_runtime_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
#[path = "tests/array_buffers.rs"]
mod array_buffers_tests;

#[cfg(test)]
#[path = "tests/trigger_initialization.rs"]
mod trigger_initialization;

#[cfg(test)]
#[path = "tests/bool_convert_heal.rs"]
mod bool_convert_heal_tests;

#[cfg(test)]
#[path = "tests/layer_skin.rs"]
mod layer_skin_tests;

#[cfg(all(test, feature = "gpu-proofs"))]
#[path = "tests/chain_fusion.rs"]
mod chain_fusion_tests;

#[cfg(test)]
#[path = "tests/segment_prewarm.rs"]
mod segment_prewarm_tests;

#[cfg(test)]
#[path = "tests/bound_param_survives_rebuild.rs"]
mod bound_param_survives_rebuild_tests;

#[cfg(test)]
#[path = "tests/modifier_events.rs"]
mod modifier_events_tests;

#[cfg(test)]
#[path = "tests/math_view.rs"]
mod math_view_tests;

#[cfg(all(test, feature = "gpu-proofs"))]
#[path = "tests/blob_grain_probe.rs"]
mod blob_grain_probe_tests;

#[cfg(all(test, feature = "gpu-proofs"))]
#[path = "tests/mosh.rs"]
mod mosh_tests;
