//! `GraphHost` — the one abstraction over an effect instance and a
//! layer's generator, so editing commands, Ableton dispatch, and the
//! card-param write path stop forking on effect-vs-generator.
//!
//! ## Why a trait, and what it deliberately does NOT unify
//!
//! Effects ([`EffectInstance`]) and generators ([`GeneratorParamState`],
//! reached through its owning [`Layer`]) are the same kind of thing — a
//! node graph that exposes params, modulates them, and round-trips to
//! disk. Everything that is *genuinely symmetric* lives behind this
//! trait: the per-instance graph override + its version, the
//! `param_values` value bus, the per-instance reshape notes
//! (`param_mappings`), and the driver / Ableton modulation reads. Those
//! fields are field-for-field identical on both structs, so the commands
//! that touch them can operate on `&mut dyn GraphHost` and exist once.
//!
//! What the trait does **not** paper over — because the differences are
//! real, not incidental — and stays kind-specific on the call sites:
//!
//! - **User param bindings.** Effects keep a sibling
//!   `EffectInstance::user_param_bindings` Vec; generators store user
//!   bindings inside `generator_graph.preset_metadata.bindings`.
//!   [`GraphHost::user_param_bindings`] returns the effect Vec and an
//!   empty slice for generators — enumeration is uniform, the storage is
//!   not, and the expose/unexpose command stays effect-only.
//! - **Envelopes.** Effect envelopes live on the owning [`Layer`], keyed
//!   by `(effect_type, param_id)` (master effects have none); generator
//!   envelopes live on [`GeneratorParamState`] itself, keyed by
//!   `param_id` alone. The trait carries no envelope accessor.
//! - **Base-param clamping.** Generators clamp writes against the
//!   registry; effects clamp upstream in the UI. Each impl of
//!   [`GraphHost::set_base_param_by_id`] keeps its own policy.
//!
//! ## A generator host can exist without param state
//!
//! Graph editing (Add/Remove/Connect/… nodes) only needs the
//! `generator_graph` override, which a generator layer always has;
//! [`GeneratorParamState`] (`gen_params`) may still be `None` on a
//! freshly-created generator layer whose graph is being edited before
//! its params are initialised. So [`GeneratorHost`] carries an *optional*
//! params handle: the graph-def methods always work, and the param
//! methods degrade to empty / no-op / `false` when params are absent.
//!
//! ## Resolution: closure, not borrow-return
//!
//! A generator host is a *temporary* ([`GeneratorHost`]) bundling disjoint
//! [`Layer`] fields, so it can't be returned as `&mut dyn GraphHost`.
//! [`crate::project::Project::with_graph_host_mut`] takes a closure
//! instead, matching the existing `with_target_graph_mut` pattern in the
//! graph editing commands.

use crate::ableton_mapping::AbletonParamMapping;
use crate::effect_graph_def::EffectGraphDef;
use crate::effects::{EffectInstance, ParamMapping, ParamSlot, ParameterDriver, UserParamBinding};
use crate::generator::GeneratorParamState;

/// Which kind of host a `&dyn GraphHost` is, for the few genuinely
/// kind-specific decisions a caller still has to make (envelope home,
/// the effect-only user-binding tier).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphHostKind {
    Effect,
    Generator,
}

/// The unified surface over an effect instance and a layer's generator.
/// See the module docs for what is and isn't behind it.
pub trait GraphHost {
    /// Discriminator for the few genuinely kind-specific call sites.
    fn host_kind(&self) -> GraphHostKind;

    // ── Per-instance graph override (always available) ────────────────
    // Effect: `EffectInstance::graph` / `graph_version`.
    // Generator: `Layer::generator_graph` / `generator_graph_version`.

    /// The per-instance graph override (`None` ⇒ use the catalog default).
    fn graph_def(&self) -> &Option<EffectGraphDef>;
    /// Mutable handle to the override, for the graph editing commands.
    fn graph_def_mut(&mut self) -> &mut Option<EffectGraphDef>;
    /// Bump the graph version so the renderer rehydrates the runtime graph.
    fn bump_graph_version(&mut self);

    // ── Value bus (`param_values`) ────────────────────────────────────

    /// The effective (post-modulation) value slots (empty if the
    /// generator has no param state yet).
    fn param_values(&self) -> &[ParamSlot];

    /// Resolve a stable `param_id` to its `param_values` slot, including
    /// the effect user-binding tail / generator `preset_metadata` tier.
    fn resolve_param_slot(&self, param_id: &str) -> Option<usize>;

    /// Read the user-set base value (pre-modulation) for a `param_id`.
    fn get_base_param_by_id(&self, param_id: &str) -> Option<f32>;

    /// Write the user-set base value for a `param_id`. Returns `true` if
    /// the id resolved (and there was param state to write). Each impl
    /// keeps its own clamp policy (generators clamp against the registry;
    /// effects don't — the UI clamps).
    fn set_base_param_by_id(&mut self, param_id: &str, value: f32) -> bool;

    /// Static-registry index for `param_id` (the declaration-order slot,
    /// excluding any user tail) — what the Ableton listener rebuild keys
    /// on. `None` for unknown / user-tail ids.
    fn param_id_to_static_index(&self, param_id: &str) -> Option<usize>;

    /// `(min, max)` for a static param slot, from this host's registry.
    /// Defaults to `(0.0, 1.0)` when the registry can't supply a range.
    fn param_range(&self, index: usize) -> (f32, f32);

    // ── Modulation stores (read; mutable accessors land with their
    //    Phase-C/E callers, shaped Option-safely there) ───────────────

    fn drivers(&self) -> Option<&Vec<ParameterDriver>>;
    fn ableton_mappings(&self) -> Option<&Vec<AbletonParamMapping>>;

    // ── Per-instance reshape notes (`param_mappings`) ─────────────────

    fn param_mapping(&self, id: &str) -> Option<&ParamMapping>;
    fn upsert_param_mapping(&mut self, mapping: ParamMapping);
    fn remove_param_mapping(&mut self, id: &str);

    // ── User param bindings (effect-only tier) ────────────────────────

    /// The effect's user-exposed bindings; an empty slice for generators
    /// (whose user bindings live in `preset_metadata.bindings`). Lets a
    /// host-generic command enumerate "is there a user binding for this
    /// id?" uniformly while the storage stays kind-specific.
    fn user_param_bindings(&self) -> &[UserParamBinding];

    /// Mutable access to a user binding by stable id, for the inline
    /// reshape edit (label / range / invert / curve / scale / offset).
    /// `None` for generators — they have no user-binding tier, so a
    /// host-generic mapping edit falls through to the reshape-note path.
    fn user_param_binding_mut(&mut self, id: &str) -> Option<&mut UserParamBinding>;

    /// Bump the user-binding version so the renderer rebuilds the user
    /// tail (and drops its `LastAppliedCache` tail) next frame. No-op for
    /// generators.
    fn bump_user_bindings_version(&mut self);
}

// ── EffectInstance: impl directly ─────────────────────────────────────

impl GraphHost for EffectInstance {
    fn host_kind(&self) -> GraphHostKind {
        GraphHostKind::Effect
    }

    fn graph_def(&self) -> &Option<EffectGraphDef> {
        &self.graph
    }
    fn graph_def_mut(&mut self) -> &mut Option<EffectGraphDef> {
        &mut self.graph
    }
    fn bump_graph_version(&mut self) {
        self.graph_version = self.graph_version.wrapping_add(1);
    }

    fn param_values(&self) -> &[ParamSlot] {
        &self.param_values
    }

    fn resolve_param_slot(&self, param_id: &str) -> Option<usize> {
        self.param_id_to_value_index(param_id)
    }

    fn get_base_param_by_id(&self, param_id: &str) -> Option<f32> {
        let idx = self.param_id_to_value_index(param_id)?;
        Some(self.get_base_param(idx))
    }

    fn set_base_param_by_id(&mut self, param_id: &str, value: f32) -> bool {
        match self.param_id_to_value_index(param_id) {
            // Effects don't clamp here — the UI clamps upstream.
            Some(idx) => {
                self.set_base_param(idx, value);
                true
            }
            None => false,
        }
    }

    fn param_id_to_static_index(&self, param_id: &str) -> Option<usize> {
        crate::effect_definition_registry::param_id_to_index(self.effect_type(), param_id)
    }

    fn param_range(&self, index: usize) -> (f32, f32) {
        crate::effect_definition_registry::try_get(self.effect_type())
            .and_then(|d| d.param_defs.get(index))
            .map(|p| (p.min, p.max))
            .unwrap_or((0.0, 1.0))
    }

    fn drivers(&self) -> Option<&Vec<ParameterDriver>> {
        self.drivers.as_ref()
    }
    fn ableton_mappings(&self) -> Option<&Vec<AbletonParamMapping>> {
        self.ableton_mappings.as_ref()
    }

    fn param_mapping(&self, id: &str) -> Option<&ParamMapping> {
        EffectInstance::param_mapping(self, id)
    }
    fn upsert_param_mapping(&mut self, mapping: ParamMapping) {
        EffectInstance::upsert_param_mapping(self, mapping)
    }
    fn remove_param_mapping(&mut self, id: &str) {
        EffectInstance::remove_param_mapping(self, id)
    }

    fn user_param_bindings(&self) -> &[UserParamBinding] {
        &self.user_param_bindings
    }

    fn user_param_binding_mut(&mut self, id: &str) -> Option<&mut UserParamBinding> {
        self.user_param_bindings.iter_mut().find(|b| b.id == id)
    }

    fn bump_user_bindings_version(&mut self) {
        self.user_param_bindings_version = self.user_param_bindings_version.wrapping_add(1);
    }
}

// ── Generator: impl on a Layer-bound wrapper ──────────────────────────

/// A generator host: the layer's `generator_graph` override (always
/// present, drives graph editing) together with an *optional*
/// [`GeneratorParamState`] handle (the param / modulation surface, absent
/// on a generator layer whose params aren't initialised yet). Constructed
/// by [`crate::layer::Layer::graph_host_mut`] from disjoint layer fields.
pub struct GeneratorHost<'a> {
    pub params: Option<&'a mut GeneratorParamState>,
    pub graph: &'a mut Option<EffectGraphDef>,
    pub graph_version: &'a mut u32,
}

impl GraphHost for GeneratorHost<'_> {
    fn host_kind(&self) -> GraphHostKind {
        GraphHostKind::Generator
    }

    fn graph_def(&self) -> &Option<EffectGraphDef> {
        self.graph
    }
    fn graph_def_mut(&mut self) -> &mut Option<EffectGraphDef> {
        self.graph
    }
    fn bump_graph_version(&mut self) {
        *self.graph_version = self.graph_version.wrapping_add(1);
    }

    fn param_values(&self) -> &[ParamSlot] {
        self.params
            .as_deref()
            .map(|p| p.param_values.as_slice())
            .unwrap_or(&[])
    }

    fn resolve_param_slot(&self, param_id: &str) -> Option<usize> {
        // Mirror of `Layer::resolve_gen_param_slot`: prefer the override
        // graph's `preset_metadata.params` (carries user-added bindings,
        // and works even before param state exists), else the static
        // generator registry (which needs the generator type from params).
        if let Some(graph) = self.graph.as_ref()
            && let Some(meta) = graph.preset_metadata.as_ref()
            && !meta.params.is_empty()
        {
            return meta.params.iter().position(|p| p.id == param_id);
        }
        let params = self.params.as_deref()?;
        crate::generator_definition_registry::param_id_to_index(
            params.generator_type(),
            param_id,
        )
    }

    fn get_base_param_by_id(&self, param_id: &str) -> Option<f32> {
        let idx = self.resolve_param_slot(param_id)?;
        Some(self.params.as_deref()?.get_param_base(idx))
    }

    fn set_base_param_by_id(&mut self, param_id: &str, value: f32) -> bool {
        let Some(idx) = self.resolve_param_slot(param_id) else {
            return false;
        };
        match self.params.as_deref_mut() {
            // Generators clamp against the registry inside `set_param_base`.
            Some(params) => {
                params.set_param_base(idx, value);
                true
            }
            None => false,
        }
    }

    fn param_id_to_static_index(&self, param_id: &str) -> Option<usize> {
        let params = self.params.as_deref()?;
        crate::generator_definition_registry::param_id_to_index(
            params.generator_type(),
            param_id,
        )
    }

    fn param_range(&self, index: usize) -> (f32, f32) {
        self.params
            .as_deref()
            .and_then(|p| {
                crate::generator_definition_registry::try_get(p.generator_type())
                    .and_then(|d| d.param_defs.get(index))
                    .map(|pd| (pd.min, pd.max))
            })
            .unwrap_or((0.0, 1.0))
    }

    fn drivers(&self) -> Option<&Vec<ParameterDriver>> {
        self.params.as_deref().and_then(|p| p.drivers.as_ref())
    }
    fn ableton_mappings(&self) -> Option<&Vec<AbletonParamMapping>> {
        self.params
            .as_deref()
            .and_then(|p| p.ableton_mappings.as_ref())
    }

    fn param_mapping(&self, id: &str) -> Option<&ParamMapping> {
        self.params.as_deref().and_then(|p| p.param_mapping(id))
    }
    fn upsert_param_mapping(&mut self, mapping: ParamMapping) {
        if let Some(params) = self.params.as_deref_mut() {
            params.upsert_param_mapping(mapping)
        }
    }
    fn remove_param_mapping(&mut self, id: &str) {
        if let Some(params) = self.params.as_deref_mut() {
            params.remove_param_mapping(id)
        }
    }

    fn user_param_bindings(&self) -> &[UserParamBinding] {
        // Generators store user bindings in `preset_metadata.bindings`,
        // not as a sibling Vec. Empty here on purpose — see module docs.
        &[]
    }

    fn user_param_binding_mut(&mut self, _id: &str) -> Option<&mut UserParamBinding> {
        None
    }

    fn bump_user_bindings_version(&mut self) {}
}
