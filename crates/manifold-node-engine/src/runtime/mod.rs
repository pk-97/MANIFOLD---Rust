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

use crate::gpu::gpu_encoder::GpuEncoder;
use crate::primitives::mix::Mix;
use crate::{param_binding::BindingSource, exec::bound_graph::BoundGraph, persistence::EffectGraphDefExt, exec::execution_plan::ExecutionPlan, exec::execution::Executor, scene::boundary_nodes::FINAL_OUTPUT_TYPE_ID, scene::boundary_nodes::FinalOutput, exec::effect_node::FrameTime, scene::boundary_nodes::GENERATOR_INPUT_TYPE_ID, graph::Graph, validation::GraphError, persistence::LoadError, load::loaded_preset_view::LoadedPresetView, exec::metal_backend::MetalBackend, exec::effect_node::NodeInstanceId, param_binding::ParamBinding, parameters::ParamValue, persistence::PrimitiveRegistry, param_binding::ResolvedBinding, param_binding::ResolvedTarget, exec::execution_plan::ResourceId, bindings::Slot, scene::boundary_nodes::Source, load::chain_spec::SpliceResult, state_store::StateStore, param_binding::apply_binding_defaults, exec::execution_plan::compile, load::chain_spec::splice_def_into_chain};
use crate::load::loaded_preset_view::loaded_preset_view_by_id;
use crate::runtime::preset_context::PresetContext;
use manifold_core::effect_graph_def::{EFFECT_GRAPH_VERSION_WITH_SCENE_MODIFIERS, EffectGraphDef};
use manifold_core::params::ParamManifest;
use manifold_core::{Beats, Seconds};
use crate::gpu::render_target::RenderTarget;

mod errors;
pub use errors::{ChainError, JsonGeneratorLoadError};
use errors::record_chain_error;

mod bindings;
use bindings::{StringBindingResolution, def_string_param_value, RelightParamWrite, build_relight_writes};

mod segments;
pub use segments::{prewarm_chain_segments, prewarm_project_chain_segments};
pub use crate::freeze::install::prewarm_worker_pending_count;
use segments::{SegmentMember, classify_segment_member, segment_run, build_segment_cards};

mod build;
mod device;
pub use build::chain_topology_hash;
use build::{assign_texture2d_slots, compute_topology_hash};

mod groups;
use groups::{chain_active_effects, close_mix_group, validate_mask_groups, OpenGroup};
pub use scene_impulses::SceneImpulseDiagnostics;
pub use physics_impulses::{CapturedSceneImpulse, PreparedSceneImpulse};
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
pub use modifier_preview::{ModifierPreviewContext, ModifierPreviewError};

#[cfg(all(test, feature = "gpu-proofs"))]
mod multi_segment_tests;

#[cfg(all(test, feature = "gpu-proofs"))]
mod group_mask_tests;

#[cfg(all(test, feature = "gpu-proofs"))]
mod binding_seed_tests;

#[cfg(test)]
mod topology_hash_tests;

#[cfg(all(test, feature = "gpu-proofs"))]
mod user_binding_tests;

#[cfg(test)]
mod bug080_manifest_gate_tests;

#[cfg(test)]
mod persistent_slot_tests;

#[cfg(test)]
mod transient_slot_tests;

#[cfg(all(test, feature = "gpu-proofs"))]
mod generator_input_tests;

#[cfg(all(test, feature = "gpu-proofs"))]
mod chain_error_tests;

#[cfg(all(test, feature = "gpu-proofs"))]
mod amount_zero_passthrough_tests;


#[cfg(test)]
mod trigger_initialization;




#[cfg(test)]
mod segment_prewarm_tests;




#[cfg(all(test, feature = "gpu-proofs"))]
mod blob_grain_probe_tests;

#[cfg(all(test, feature = "gpu-proofs"))]
mod mosh_tests;

#[cfg(test)]
#[doc(hidden)]
pub(crate) mod testkit;
pub mod background_worker;
pub mod chain_dispatch;
pub mod effect;
pub mod effects;
pub mod frame_status;
pub mod layer_skin;
pub mod plugin_prewarm;
pub mod preset_context;
