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
//! (`param_mappings`), and the driver / Ableton modulation stores. Those
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
//!   `param_id` alone. The trait carries no envelope accessor — a
//!   `&mut dyn GraphHost` resolved to an effect can't reach its host
//!   layer, so envelope round-tripping stays at the command layer.
//! - **Base-param clamping.** Generators clamp writes against the
//!   registry; effects clamp upstream in the UI. Each impl of
//!   [`GraphHost::set_base_param_by_id`] keeps its own policy.
//!
//! ## Resolution: closure, not borrow-return
//!
//! A generator host is a *temporary* ([`GeneratorHost`]) bundling two
//! disjoint [`Layer`] fields, so it can't be returned as
//! `&mut dyn GraphHost`. [`crate::project::Project::with_graph_host_mut`]
//! takes a closure instead, matching the existing
//! `with_target_graph_mut` pattern in the graph editing commands.

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

    // ── Per-instance graph override ───────────────────────────────────
    // Effect: `EffectInstance::graph` / `graph_version`.
    // Generator: `Layer::generator_graph` / `generator_graph_version`.

    /// The per-instance graph override (`None` ⇒ use the catalog default).
    fn graph_def(&self) -> &Option<EffectGraphDef>;
    /// Mutable handle to the override, for the graph editing commands.
    fn graph_def_mut(&mut self) -> &mut Option<EffectGraphDef>;
    /// Bump the graph version so the renderer rehydrates the runtime graph.
    fn bump_graph_version(&mut self);

    // ── Value bus (`param_values`) ────────────────────────────────────

    /// The effective (post-modulation) value slots.
    fn param_values(&self) -> &[ParamSlot];
    /// Mutable value slots — for the host-generic show/hide-exposed edit.
    fn param_values_mut(&mut self) -> &mut Vec<ParamSlot>;

    /// Resolve a stable `param_id` to its `param_values` slot, including
    /// the effect user-binding tail / generator `preset_metadata` tier.
    fn resolve_param_slot(&self, param_id: &str) -> Option<usize>;

    /// Read the user-set base value (pre-modulation) for a `param_id`.
    fn get_base_param_by_id(&self, param_id: &str) -> Option<f32>;

    /// Write the user-set base value for a `param_id`. Returns `true` if
    /// the id resolved. Each impl keeps its own clamp policy (generators
    /// clamp against the registry; effects don't — the UI clamps).
    fn set_base_param_by_id(&mut self, param_id: &str, value: f32) -> bool;

    /// Static-registry index for `param_id` (the declaration-order slot,
    /// excluding any user tail) — what the Ableton listener rebuild keys
    /// on. `None` for unknown / user-tail ids.
    fn param_id_to_static_index(&self, param_id: &str) -> Option<usize>;

    /// `(min, max)` for a static param slot, from this host's registry.
    /// Defaults to `(0.0, 1.0)` when the registry can't supply a range.
    fn param_range(&self, index: usize) -> (f32, f32);

    // ── Modulation stores (field-for-field identical on both) ─────────

    fn drivers(&self) -> Option<&Vec<ParameterDriver>>;
    fn drivers_mut(&mut self) -> &mut Option<Vec<ParameterDriver>>;
    fn ableton_mappings(&self) -> Option<&Vec<AbletonParamMapping>>;
    fn ableton_mappings_mut(&mut self) -> &mut Option<Vec<AbletonParamMapping>>;

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
    fn param_values_mut(&mut self) -> &mut Vec<ParamSlot> {
        &mut self.param_values
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
    fn drivers_mut(&mut self) -> &mut Option<Vec<ParameterDriver>> {
        &mut self.drivers
    }
    fn ableton_mappings(&self) -> Option<&Vec<AbletonParamMapping>> {
        self.ableton_mappings.as_ref()
    }
    fn ableton_mappings_mut(&mut self) -> &mut Option<Vec<AbletonParamMapping>> {
        &mut self.ableton_mappings
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
}

// ── Generator: impl on a Layer-bound wrapper ──────────────────────────

/// A generator host: a [`GeneratorParamState`] together with its owning
/// layer's `generator_graph` override (needed for `preset_metadata`-aware
/// slot resolution and for the graph editing commands). Constructed by
/// [`crate::layer::Layer::graph_host_mut`] from disjoint layer fields.
pub struct GeneratorHost<'a> {
    pub params: &'a mut GeneratorParamState,
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
        &self.params.param_values
    }
    fn param_values_mut(&mut self) -> &mut Vec<ParamSlot> {
        &mut self.params.param_values
    }

    fn resolve_param_slot(&self, param_id: &str) -> Option<usize> {
        // Mirror of `Layer::resolve_gen_param_slot`: prefer the override
        // graph's `preset_metadata.params` (which carries user-added
        // bindings), else the static generator registry.
        if let Some(graph) = self.graph.as_ref()
            && let Some(meta) = graph.preset_metadata.as_ref()
            && !meta.params.is_empty()
        {
            return meta.params.iter().position(|p| p.id == param_id);
        }
        crate::generator_definition_registry::param_id_to_index(
            self.params.generator_type(),
            param_id,
        )
    }

    fn get_base_param_by_id(&self, param_id: &str) -> Option<f32> {
        let idx = self.resolve_param_slot(param_id)?;
        Some(self.params.get_param_base(idx))
    }

    fn set_base_param_by_id(&mut self, param_id: &str, value: f32) -> bool {
        match self.resolve_param_slot(param_id) {
            // Generators clamp against the registry inside `set_param_base`.
            Some(idx) => {
                self.params.set_param_base(idx, value);
                true
            }
            None => false,
        }
    }

    fn param_id_to_static_index(&self, param_id: &str) -> Option<usize> {
        crate::generator_definition_registry::param_id_to_index(
            self.params.generator_type(),
            param_id,
        )
    }

    fn param_range(&self, index: usize) -> (f32, f32) {
        crate::generator_definition_registry::try_get(self.params.generator_type())
            .and_then(|d| d.param_defs.get(index))
            .map(|p| (p.min, p.max))
            .unwrap_or((0.0, 1.0))
    }

    fn drivers(&self) -> Option<&Vec<ParameterDriver>> {
        self.params.drivers.as_ref()
    }
    fn drivers_mut(&mut self) -> &mut Option<Vec<ParameterDriver>> {
        &mut self.params.drivers
    }
    fn ableton_mappings(&self) -> Option<&Vec<AbletonParamMapping>> {
        self.params.ableton_mappings.as_ref()
    }
    fn ableton_mappings_mut(&mut self) -> &mut Option<Vec<AbletonParamMapping>> {
        &mut self.params.ableton_mappings
    }

    fn param_mapping(&self, id: &str) -> Option<&ParamMapping> {
        self.params.param_mapping(id)
    }
    fn upsert_param_mapping(&mut self, mapping: ParamMapping) {
        self.params.upsert_param_mapping(mapping)
    }
    fn remove_param_mapping(&mut self, id: &str) {
        self.params.remove_param_mapping(id)
    }

    fn user_param_bindings(&self) -> &[UserParamBinding] {
        // Generators store user bindings in `preset_metadata.bindings`,
        // not as a sibling Vec. Empty here on purpose — see module docs.
        &[]
    }
}
