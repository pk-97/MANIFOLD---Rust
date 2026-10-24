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
use manifold_core::NodeId;
use manifold_core::PresetTypeId;
use manifold_core::effects::{EffectGroup, PresetInstance};
use manifold_core::id::{EffectGroupId, EffectId};
use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureFormat, TexturePool};

use crate::gpu::gpu_encoder::GpuEncoder;
use crate::gpu::render_target::RenderTarget;
use crate::load::loaded_preset_view::loaded_preset_view_by_id;
use crate::primitives::mix::Mix;
use crate::runtime::preset_context::PresetContext;
use crate::{
    bindings::Slot, exec::bound_graph::BoundGraph, exec::effect_node::FrameTime,
    exec::effect_node::NodeInstanceId, exec::execution::Executor,
    exec::execution_plan::ExecutionPlan, exec::execution_plan::ResourceId,
    exec::execution_plan::compile, exec::metal_backend::MetalBackend, graph::Graph,
    load::chain_spec::SpliceResult, load::chain_spec::splice_def_into_chain,
    load::loaded_preset_view::LoadedPresetView, param_binding::BindingSource,
    param_binding::ParamBinding, param_binding::ResolvedBinding, param_binding::ResolvedTarget,
    param_binding::apply_binding_defaults, parameters::ParamValue, persistence::EffectGraphDefExt,
    persistence::LoadError, persistence::PrimitiveRegistry,
    scene::boundary_nodes::FINAL_OUTPUT_TYPE_ID, scene::boundary_nodes::FinalOutput,
    scene::boundary_nodes::GENERATOR_INPUT_TYPE_ID, scene::boundary_nodes::Source,
    state_store::StateStore, validation::GraphError,
};
use manifold_core::effect_graph_def::{EFFECT_GRAPH_VERSION_WITH_SCENE_MODIFIERS, EffectGraphDef};
use manifold_core::params::ParamManifest;
use manifold_core::{Beats, Seconds};

mod errors;
pub mod generator_provider;
mod scene_events;
mod selection_mask;
use errors::record_chain_error;
pub use errors::{ChainError, JsonGeneratorLoadError};

mod bindings;
use bindings::{StringBindingResolution, def_string_param_value};

manifold_core::testkit_visible! {
#[doc(hidden)]
pub(crate) mod segments;
}
pub use crate::freeze::install::prewarm_worker_pending_count;
use segments::{SegmentMember, build_segment_cards, classify_segment_member, segment_run};
pub use segments::{prewarm_chain_segments, prewarm_project_chain_segments};
mod segment_bindings;
use segment_bindings::{SegmentRefusal, resolve_segment_bindings};

manifold_core::testkit_visible! {
#[doc(hidden)]
pub(crate) mod build;
}
mod binding_shape;
mod device;
pub use build::chain_topology_hash;
use build::{assign_texture2d_slots, compute_topology_hash};

mod groups;
use groups::{
    OpenGroup, chain_active_effects, close_mix_group, has_chain_active_effects,
    validate_mask_groups,
};
mod convert_heal;
pub mod extensions;
mod lifecycle;

manifold_core::testkit_visible! {
pub(crate) mod core;
}
pub use self::core::{ChainBuildInputs, FrameContextInputs, PresetRuntime};
mod resize;
pub use resize::PreparedRuntimeResize;
mod debug;
use self::core::{EffectSlot, PresetIo};
pub use debug::{ChainDebugInfo, StepDebugInfo};

pub mod beat_envelope;
mod dump_sets;
pub mod instrumentation;
mod modifier_preview;
mod modifier_runtime;
mod scene_viewport;
pub use modifier_preview::{ModifierPreviewContext, ModifierPreviewError};

#[cfg(test)]
mod topology_hash_tests;

#[cfg(test)]
mod persistent_slot_tests;

#[cfg(test)]
mod transient_slot_tests;

#[cfg(test)]
mod segment_bindings_tests;

#[cfg(test)]
mod groups_tests;

pub mod background_worker;
pub mod chain_dispatch;
pub mod effect;
pub mod frame_status;
pub mod layer_skin;
pub mod plugin_prewarm;
pub mod preset_context;
#[cfg(any(test, feature = "testkit"))]
#[doc(hidden)]
pub mod testkit;
