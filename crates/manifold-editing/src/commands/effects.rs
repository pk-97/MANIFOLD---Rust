use crate::command::Command;
use crate::commands::effect_target::{EffectTarget, with_effects_mut};
use manifold_core::{EffectId, GraphTarget};
use manifold_core::effects::{
    PresetInstance, ParamConvert, ParamEnvelope, ParamId, ParamSlot, ParameterDriver,
    UserParamBinding,
};
use manifold_core::project::Project;

// ── Addressing model ──────────────────────────────────────────────────
//
// Two distinct targeting concepts, deliberately kept apart:
//
// * Single-effect edits (toggle, param value, expose, binding mapping) address
//   ONE instance by its stable [`EffectId`] and resolve it via
//   `Project::find_effect_by_id_mut`, which searches master + every layer +
//   every clip. So a card edit reaches the right instance regardless of where
//   it lives — no positional index, no ambient "active layer" read.
//
// * List / structural ops (add, remove, reorder, group-reorder) address an
//   effect *list* by [`EffectTarget`] (master or a layer) — there is no single
//   instance yet for an insert, so an id can't name the destination. That is
//   `EffectTarget`'s sole remaining role.

/// Add an effect to a target's effect chain.
#[derive(Debug)]
pub struct AddEffectCommand {
    target: EffectTarget,
    effect: PresetInstance,
    insert_index: usize,
}

impl AddEffectCommand {
    pub fn new(target: EffectTarget, effect: PresetInstance, insert_index: usize) -> Self {
        Self {
            target,
            effect,
            insert_index,
        }
    }
}

impl Command for AddEffectCommand {
    fn execute(&mut self, project: &mut Project) {
        with_effects_mut(project, &self.target, |effects, _groups| {
            let idx = self.insert_index.min(effects.len());
            effects.insert(idx, self.effect.clone());
        });
    }

    fn undo(&mut self, project: &mut Project) {
        with_effects_mut(project, &self.target, |effects, _groups| {
            let idx = self.insert_index.min(effects.len().saturating_sub(1));
            if idx < effects.len() {
                effects.remove(idx);
            }
        });
    }

    fn description(&self) -> &str {
        "Add Effect"
    }
}

/// Remove an effect from a target's effect chain.
#[derive(Debug)]
pub struct RemoveEffectCommand {
    target: EffectTarget,
    effect: Option<PresetInstance>,
    removed_index: usize,
}

impl RemoveEffectCommand {
    pub fn new(target: EffectTarget, effect: PresetInstance, removed_index: usize) -> Self {
        Self {
            target,
            effect: Some(effect),
            removed_index,
        }
    }
}

impl Command for RemoveEffectCommand {
    fn execute(&mut self, project: &mut Project) {
        with_effects_mut(project, &self.target, |effects, _groups| {
            if self.removed_index < effects.len() {
                self.effect = Some(effects.remove(self.removed_index));
            }
        });
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(effect) = &self.effect {
            let effect = effect.clone();
            let idx = self.removed_index;
            with_effects_mut(project, &self.target, |effects, _groups| {
                let insert_idx = idx.min(effects.len());
                effects.insert(insert_idx, effect);
            });
        }
    }

    fn description(&self) -> &str {
        "Remove Effect"
    }
}

/// Reorder an effect within a target's effect chain.
#[derive(Debug)]
pub struct ReorderEffectCommand {
    target: EffectTarget,
    from_index: usize,
    to_index: usize,
}

impl ReorderEffectCommand {
    pub fn new(target: EffectTarget, from_index: usize, to_index: usize) -> Self {
        Self {
            target,
            from_index,
            to_index,
        }
    }
}

impl Command for ReorderEffectCommand {
    fn execute(&mut self, project: &mut Project) {
        let from = self.from_index;
        let to = self.to_index;
        with_effects_mut(project, &self.target, |effects, _groups| {
            if from < effects.len() {
                let effect = effects.remove(from);
                // After remove, indices shift: if to > from, the target shifted down by 1
                let insert_idx = if to > from { to - 1 } else { to };
                let insert_idx = insert_idx.min(effects.len());
                effects.insert(insert_idx, effect);
            }
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let from = self.from_index;
        let to = self.to_index;
        with_effects_mut(project, &self.target, |effects, _groups| {
            // Reverse of execute: the item is now at adjusted_to
            let adjusted_to = if to > from { to - 1 } else { to };
            let adjusted_to = adjusted_to.min(effects.len().saturating_sub(1));
            if adjusted_to < effects.len() {
                let effect = effects.remove(adjusted_to);
                let insert_idx = from.min(effects.len());
                effects.insert(insert_idx, effect);
            }
        });
    }

    fn description(&self) -> &str {
        "Reorder Effect"
    }
}

/// Toggle an effect's enabled state. Addressed by stable [`EffectId`].
#[derive(Debug)]
pub struct ToggleEffectCommand {
    effect_id: EffectId,
    old_enabled: bool,
    new_enabled: bool,
}

impl ToggleEffectCommand {
    pub fn new(effect_id: EffectId, old_enabled: bool, new_enabled: bool) -> Self {
        Self {
            effect_id,
            old_enabled,
            new_enabled,
        }
    }
}

impl Command for ToggleEffectCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(effect) = project.find_effect_by_id_mut(&self.effect_id) {
            effect.enabled = self.new_enabled;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(effect) = project.find_effect_by_id_mut(&self.effect_id) {
            effect.enabled = self.old_enabled;
        }
    }

    fn description(&self) -> &str {
        "Toggle Effect"
    }
}

/// Reorder a group of effects within a target's effect chain (multi-select).
#[derive(Debug)]
pub struct ReorderEffectGroupCommand {
    target: EffectTarget,
    /// Snapshot of the entire effects vec before the reorder.
    old_effects: Vec<PresetInstance>,
    /// Snapshot of the entire effects vec after the reorder.
    new_effects: Vec<PresetInstance>,
}

impl ReorderEffectGroupCommand {
    /// Construct from before/after snapshots of the effects vec.
    pub fn new(
        target: EffectTarget,
        old_effects: Vec<PresetInstance>,
        new_effects: Vec<PresetInstance>,
    ) -> Self {
        Self {
            target,
            old_effects,
            new_effects,
        }
    }
}

impl Command for ReorderEffectGroupCommand {
    fn execute(&mut self, project: &mut Project) {
        with_effects_mut(project, &self.target, |effects, _groups| {
            *effects = self.new_effects.clone();
        });
    }

    fn undo(&mut self, project: &mut Project) {
        with_effects_mut(project, &self.target, |effects, _groups| {
            *effects = self.old_effects.clone();
        });
    }

    fn description(&self) -> &str {
        "Reorder Effects"
    }
}

/// Change a single parameter value on an effect or a generator.
///
/// Addresses the parameter by stable [`ParamId`] (not by position) and the
/// host by [`GraphTarget`]. The id is resolved against the host's
/// registry/tier on each `execute`/`undo` via
/// [`manifold_core::GraphHost::set_base_param_by_id`], so an undo entry
/// stays correct even if the param list is reordered between recording and
/// replaying. Each host keeps its own clamp policy (generators clamp
/// against the registry inside `set_base_param_by_id`; effects clamp in the
/// UI). Replaces the former `ChangeEffectParamCommand` (effects) and
/// `ChangeGeneratorParamsCommand` (the generator whole-vector command,
/// whose every caller edited exactly one slot — by-id is the collapse and
/// a correctness upgrade).
#[derive(Debug)]
pub struct ChangeGraphParamCommand {
    target: GraphTarget,
    param_id: ParamId,
    old_value: f32,
    new_value: f32,
}

impl ChangeGraphParamCommand {
    pub fn new(
        target: GraphTarget,
        param_id: impl Into<ParamId>,
        old_value: f32,
        new_value: f32,
    ) -> Self {
        Self {
            target,
            param_id: param_id.into(),
            old_value,
            new_value,
        }
    }
}

impl Command for ChangeGraphParamCommand {
    fn execute(&mut self, project: &mut Project) {
        let id = self.param_id.clone();
        let val = self.new_value;
        project.with_preset_graph_mut(&self.target, |host| {
            host.set_base_param_by_id(id.as_ref(), val);
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let id = self.param_id.clone();
        let val = self.old_value;
        project.with_preset_graph_mut(&self.target, |host| {
            host.set_base_param_by_id(id.as_ref(), val);
        });
    }

    fn description(&self) -> &str {
        "Change Param"
    }
}

// ─── User-exposed parameter binding (Phase 3) ─────────────────

/// Per-effect inner-node parameter description captured at command
/// build time. Lets the command construct a [`UserParamBinding`]
/// without needing the renderer registry on the content thread.
///
/// On Unexpose, the dispatcher passes this same metadata so undo of
/// an Unexpose can rebuild the original binding.
#[derive(Debug, Clone)]
pub struct InnerParamMeta {
    pub label: String,
    pub min: f32,
    pub max: f32,
    pub default_value: f32,
    pub convert: ParamConvert,
    /// Angle presentation hint (from `ParamType::Angle`). Display-only;
    /// flows onto the appended `UserParamBinding` so the card shows degrees.
    pub is_angle: bool,
}

/// Generate the canonical user-binding id for a given inner-node
/// addressing under a particular effect instance's existing bindings.
///
/// Algorithm: `"user.<short_handle>.<inner_param>.<n>"` where `<n>`
/// is the smallest positive integer that doesn't collide with any
/// existing binding's id on this effect. Linear probe — typical N is
/// small (<10 user bindings per effect).
///
/// Two collisions across effects with different `node_handle` /
/// `inner_param` produce different prefixes and thus never collide.
pub fn generate_user_param_id(
    inner_node_handle: &str,
    inner_param: &str,
    existing_ids: &[String],
) -> String {
    // Strip any "primitive."/"composite." prefix if it ever leaks in
    // — handles are short by design, but be defensive in case future
    // composites use dotted handles.
    let short = inner_node_handle
        .rsplit_once('.')
        .map(|(_, t)| t)
        .unwrap_or(inner_node_handle);
    let prefix = format!("user.{short}.{inner_param}");
    let mut n: u32 = 1;
    loop {
        let candidate = format!("{prefix}.{n}");
        if !existing_ids.contains(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Toggle whether an inner-graph parameter is user-exposed on an
/// [`PresetInstance`]. One command for both directions (expose /
/// unexpose) so a single undo entry covers a single user click.
///
/// On execute, [`Self::reverse`] gets populated with the inverse
/// state needed to undo: the assigned `user_param_id` for an Expose,
/// or the removed binding + slot values for an Unexpose. Re-executing
/// after undo is symmetric (deterministic id generation, idempotent
/// against the live binding state).
///
/// See `docs/EFFECT_RUNTIME_UNIFICATION.md` §7.6.
#[derive(Debug)]
pub struct ToggleEffectParamExposeCommand {
    effect_id: EffectId,
    /// Stable [`NodeId`] of the inner node — the addressing identity the
    /// command keys both directions on (Expose creates a binding
    /// targeting this id; Unexpose removes the binding matching it).
    /// Invariant under grouping, so the toggle stays correct after the
    /// node's handle changes.
    node_id: manifold_core::NodeId,
    /// Current display handle of the node, used only to mint a readable
    /// `user_param_id` (`user.<handle>.<param>.<n>`) at expose time. Not
    /// an addressing role — the id is frozen once minted, and resolution
    /// keys off [`Self::node_id`].
    node_handle: String,
    inner_param: String,
    /// Direction of the toggle. Set at command build time from the
    /// PanelAction's `expose` flag.
    expose: bool,
    /// Inner ParamDef metadata, required for Expose (to build the
    /// new `UserParamBinding`) and used by Unexpose-undo (to rebuild
    /// the binding if its convert variant is needed).
    inner_meta: InnerParamMeta,
    /// Reverse state, populated on first execute(). Persists across
    /// undo/redo so re-executing produces the same id.
    reverse: ReverseState,
}

// The `Unexposed` variant captures the full removed `UserParamBinding`
// plus every pruned driver / Ableton mapping / envelope so undo can
// restore the pre-unexpose state verbatim — that makes it much larger
// than `Exposed`. Boxing the payload would only shrink the enum on the
// undo stack (capped at 200 entries, never a render hot path), so the
// indirection isn't worth it. Same call as the sibling `NodeExposeReverse`
// in `commands/graph.rs`.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Default)]
enum ReverseState {
    /// Pre-execute or no-op state (the operation produced no change
    /// because the state was already what the user requested).
    #[default]
    None,
    /// execute() exposed: appended a new binding with this id at the
    /// tail. undo() removes by id. redo() re-appends with the same id
    /// (deterministic generator skips holes; if undo cleared the
    /// binding, the same id is regenerated cleanly).
    Exposed { user_param_id: String },
    /// execute() unexposed: removed this binding from this position
    /// with these values, plus any drivers / envelopes / Ableton
    /// mappings that referenced the binding's `param_id`. Those
    /// references would otherwise become orphans — still in their
    /// vec, never matched by `find_driver` / envelope evaluator /
    /// Ableton router, never applied. Pruning at un-expose makes the
    /// data model honest; capturing the pruned entries here lets
    /// undo restore them verbatim so re-exposing reinstates every
    /// modulation surface the user had configured.
    Unexposed {
        binding: UserParamBinding,
        slot_value: ParamSlot,
        slot_base_value: Option<f32>,
        position: usize,
        /// Drivers pruned from `PresetInstance.drivers` because their
        /// `param_id` matched the removed binding's id.
        removed_drivers: Vec<ParameterDriver>,
        /// Ableton mappings pruned from
        /// `PresetInstance.ableton_mappings` for the same reason.
        removed_ableton_mappings: Vec<manifold_core::ableton_mapping::AbletonParamMapping>,
        /// Envelopes pruned from the host (Layer for layer-targeted
        /// effects; Master has no envelope storage). `target_effect_type`
        /// must also match the effect's type id, since envelopes live
        /// on the layer and are addressed by `(effect_type, param_id)`.
        removed_envelopes: Vec<ParamEnvelope>,
    },
}

impl ToggleEffectParamExposeCommand {
    pub fn new(
        effect_id: EffectId,
        node_id: manifold_core::NodeId,
        node_handle: String,
        inner_param: String,
        expose: bool,
        inner_meta: InnerParamMeta,
    ) -> Self {
        Self {
            effect_id,
            node_id,
            node_handle,
            inner_param,
            expose,
            inner_meta,
            reverse: ReverseState::None,
        }
    }
}

impl Command for ToggleEffectParamExposeCommand {
    fn execute(&mut self, project: &mut Project) {
        let node_id = self.node_id.clone();
        let node_handle = self.node_handle.clone();
        let inner_param = self.inner_param.clone();
        let expose = self.expose;
        let meta = self.inner_meta.clone();
        // Resolve the instance by id (master / layer / clip). `None` when the
        // id doesn't resolve — leaves `reverse` as `None`, a clean no-op.
        let reverse_out = project.find_effect_by_id_mut(&self.effect_id).map(|effect| {
            // Locate any existing binding for this (node_id, inner_param).
            // User bindings are synthesized from the graph's user-added
            // metadata — the single binding-storage list.
            let user_bindings = effect.user_param_bindings();
            let existing_position = user_bindings
                .iter()
                .position(|b| b.node_id == node_id && b.inner_param == inner_param);

            if expose {
                // Idempotent: if already exposed, no-op.
                if existing_position.is_some() {
                    return ReverseState::None;
                }
                let existing_ids: Vec<String> =
                    user_bindings.iter().map(|b| b.id.clone()).collect();
                let id =
                    generate_user_param_id(&node_handle, &inner_param, &existing_ids);
                let binding = UserParamBinding {
                    id: id.clone(),
                    label: meta.label.clone(),
                    node_id: node_id.clone(),
                    legacy_node_handle: None,
                    inner_param: inner_param.clone(),
                    min: meta.min,
                    max: meta.max,
                    default_value: meta.default_value,
                    convert: meta.convert,
                    is_angle: meta.is_angle,
                    invert: false,
                    curve: Default::default(),
                    scale: 1.0,
                    offset: 0.0,
                };
                effect.append_user_binding(binding);
                ReverseState::Exposed { user_param_id: id }
            } else {
                // Unexpose: remove the binding matching this addressing.
                let Some(position) = existing_position else {
                    return ReverseState::None;
                };
                let user_param_id = user_bindings[position].id.clone();
                // Read the current slot values BEFORE removal so undo
                // can reinstate them.
                let value_idx = effect.param_id_to_value_index(&user_param_id);
                let slot_value = value_idx
                    .and_then(|i| effect.param_values.get(i).copied())
                    .unwrap_or(ParamSlot::exposed(meta.default_value));
                let slot_base_value = value_idx.and_then(|i| {
                    effect
                        .base_param_values
                        .as_ref()
                        .and_then(|b| b.get(i).copied())
                });
                // Prune effect-local modulation references that targeted
                // this binding. After the binding goes away its id stops
                // resolving anywhere, so the driver / Ableton row would
                // just be an orphan — visible in the project file, never
                // applied, never editable. Capture pruned entries on
                // the reverse state so undo restores them verbatim.
                let removed_drivers = if let Some(ds) = effect.drivers.as_mut() {
                    let mut taken = Vec::new();
                    ds.retain(|d| {
                        let keep = d.param_id != user_param_id;
                        if !keep {
                            taken.push(d.clone());
                        }
                        keep
                    });
                    if ds.is_empty() {
                        effect.drivers = None;
                    }
                    taken
                } else {
                    Vec::new()
                };
                let removed_ableton_mappings =
                    if let Some(ms) = effect.ableton_mappings.as_mut() {
                        let mut taken = Vec::new();
                        ms.retain(|m| {
                            let keep = m.param_id != user_param_id;
                            if !keep {
                                taken.push(m.clone());
                            }
                            keep
                        });
                        if ms.is_empty() {
                            effect.ableton_mappings = None;
                        }
                        taken
                    } else {
                        Vec::new()
                    };
                let binding = effect
                    .remove_user_binding_by_id(&user_param_id)
                    .expect("position checked above");
                ReverseState::Unexposed {
                    binding,
                    slot_value,
                    slot_base_value,
                    position,
                    removed_drivers,
                    removed_ableton_mappings,
                    // Envelope storage lives on the layer, not the
                    // effect — populated below outside the
                    // `with_effects_mut` closure.
                    removed_envelopes: Vec::new(),
                }
            }
        });
        self.reverse = reverse_out.unwrap_or_default();

        // Envelope cleanup happens outside `with_effects_mut` because
        // envelopes are stored on the host (Layer for layer-targeted
        // effects; Master has no envelope storage). We need the layer
        // borrow, not the effect borrow. Only runs in the
        // `Unexposed` branch — Exposed and None leave envelopes alone.
        if let ReverseState::Unexposed {
            ref binding,
            ref mut removed_envelopes,
            ..
        } = self.reverse
        {
            let removed_id = binding.id.clone();
            // Capture the effect type here so the envelope match below
            // can key on `(target_effect_type, param_id)` — envelopes
            // address an effect by its type id within the layer's
            // chain, not by index. Envelope storage is layer-only, so the
            // owning layer is resolved by id; a master effect resolves to
            // `None` and this whole block no-ops (master has no envelopes).
            let effect_type = project
                .find_effect_by_id(&self.effect_id)
                .map(|fx| fx.effect_type().clone());
            let owning_layer = project.layer_id_for_effect(&self.effect_id);
            if let (Some(effect_type), Some(layer_id)) = (effect_type, owning_layer)
                && let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&layer_id)
            {
                let envs = layer.envelopes_mut();
                let mut taken = Vec::new();
                envs.retain(|e| {
                    let keep = !(e.target_effect_type == effect_type && e.param_id == removed_id);
                    if !keep {
                        taken.push(e.clone());
                    }
                    keep
                });
                *removed_envelopes = taken;
            }
        }
    }

    fn undo(&mut self, project: &mut Project) {
        let reverse = std::mem::take(&mut self.reverse);
        // Pull the envelope tail off the reverse state up-front;
        // restoring them happens against the layer, not the effect,
        // and we want the value before `reverse` is moved into the
        // match below.
        let envelopes_to_restore: Vec<ParamEnvelope> = match &reverse {
            ReverseState::Unexposed {
                removed_envelopes, ..
            } => removed_envelopes.clone(),
            _ => Vec::new(),
        };
        if let Some(effect) = project.find_effect_by_id_mut(&self.effect_id) {
            match reverse {
                ReverseState::None => {}
                ReverseState::Exposed { user_param_id } => {
                    effect.remove_user_binding_by_id(&user_param_id);
                }
                ReverseState::Unexposed {
                    binding,
                    slot_value,
                    slot_base_value,
                    position,
                    removed_drivers,
                    removed_ableton_mappings,
                    removed_envelopes: _,
                } => {
                    // Re-insert at original position so the user-tail
                    // slot positions stay stable for any other addressing
                    // (drivers, Ableton mappings) that referenced the
                    // user_param_id by string.
                    let binding_id = binding.id.clone();
                    let restore_slot = slot_value;
                    effect.restore_user_binding_at(binding, position, restore_slot);
                    if let Some(value_idx) = effect.param_id_to_value_index(&binding_id) {
                        // `restore_user_binding_at` seeded base from the
                        // value slot; override with the captured base if
                        // present so the pre-modulation snapshot is exact.
                        if let (Some(base), Some(base_v)) =
                            (effect.base_param_values.as_mut(), slot_base_value)
                            && value_idx < base.len()
                        {
                            base[value_idx] = base_v;
                        }
                    }
                    // Restore the drivers / Ableton mappings that
                    // execute() pruned. Their `param_id` still points
                    // at the just-reinserted binding's id, so they'll
                    // match again on the next modulation pass.
                    if !removed_drivers.is_empty() {
                        effect.drivers.get_or_insert_with(Vec::new).extend(removed_drivers);
                    }
                    if !removed_ableton_mappings.is_empty() {
                        effect
                            .ableton_mappings
                            .get_or_insert_with(Vec::new)
                            .extend(removed_ableton_mappings);
                    }
                }
            }
        }
        // Envelope restore — the owning layer is resolved by id. Master
        // effects had nothing to capture (no envelope storage) and resolve
        // to `None`, so this branch no-ops uniformly there.
        if !envelopes_to_restore.is_empty()
            && let Some(layer_id) = project.layer_id_for_effect(&self.effect_id)
            && let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&layer_id)
        {
            layer.envelopes_mut().extend(envelopes_to_restore);
        }
    }

    fn description(&self) -> &str {
        if self.expose {
            "Expose Effect Param"
        } else {
            "Unexpose Effect Param"
        }
    }
}

/// Toggle the `exposed` flag on a static-block param slot. Hidden slots
/// disappear from the effect card slider list but keep their value,
/// driver, and Ableton mapping intact — they're still addressable by
/// `param_id` for OSC / driver / mapping write paths.
///
/// Symmetric undo: stores the previous flag value (which is just `!new`
/// for non-no-op execution). No-op when the slot already matches the
/// requested state, or when `param_index` is out of bounds.
#[derive(Debug)]
pub struct ToggleStaticParamExposeCommand {
    effect_id: EffectId,
    param_index: usize,
    new_exposed: bool,
    /// Captured on first execute(). `None` when the call was a no-op.
    prev_exposed: Option<bool>,
}

impl ToggleStaticParamExposeCommand {
    pub fn new(effect_id: EffectId, param_index: usize, new_exposed: bool) -> Self {
        Self {
            effect_id,
            param_index,
            new_exposed,
            prev_exposed: None,
        }
    }
}

impl Command for ToggleStaticParamExposeCommand {
    fn execute(&mut self, project: &mut Project) {
        let pidx = self.param_index;
        let new_v = self.new_exposed;
        self.prev_exposed = project.find_effect_by_id_mut(&self.effect_id).and_then(|effect| {
            let slot = effect.param_values.get_mut(pidx)?;
            if slot.exposed == new_v {
                return None;
            }
            let was = slot.exposed;
            slot.exposed = new_v;
            Some(was)
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(prev) = self.prev_exposed else {
            return;
        };
        let pidx = self.param_index;
        if let Some(effect) = project.find_effect_by_id_mut(&self.effect_id)
            && let Some(slot) = effect.param_values.get_mut(pidx)
        {
            slot.exposed = prev;
        }
    }

    fn description(&self) -> &str {
        if self.new_exposed {
            "Show Effect Param"
        } else {
            "Hide Effect Param"
        }
    }
}

// ─── User param-binding mapping edit (card-slider reshape) ────

/// A partial edit to a [`UserParamBinding`]'s card-mapping fields.
/// Only the fields the user actually touched are `Some`; the rest stay
/// `None` and are left untouched on the binding. Used in both
/// directions by [`EditParamMappingCommand`] — `new` carries the
/// post-edit values, `reverse` (captured on first execute) carries the
/// pre-edit values for the same set of fields.
///
/// `id`, `node_handle`, `inner_param`, `default_value`, `convert`, and
/// `is_angle` are deliberately absent: the id is the forever-stable
/// addressing key that drivers / Ableton / OSC reference, and the rest
/// describe the inner-graph routing, none of which a mapping edit may
/// touch.
#[derive(Debug, Clone, Default)]
pub struct BindingMappingEdit {
    pub label: Option<String>,
    pub min: Option<f32>,
    pub max: Option<f32>,
    pub invert: Option<bool>,
    pub curve: Option<manifold_core::macro_bank::MacroCurve>,
    /// Card→consumer linear remap. This is where an in-graph
    /// `affine_scalar` that only rescaled a card value toward its
    /// consumer folds in: `out = value * scale + offset`. `scale = 1.0`,
    /// `offset = 0.0` is identity.
    pub scale: Option<f32>,
    pub offset: Option<f32>,
}

impl BindingMappingEdit {
    /// Apply each touched field onto the preset's authoring surface: range +
    /// curve + invert + label go on the `ParamSpecDef`, scale/offset on the
    /// matching `BindingDef`. The preset is the single reshape source now (no
    /// per-instance note). Returns the PRE-edit values for undo.
    fn apply_to_spec(
        &self,
        spec: &mut manifold_core::effect_graph_def::ParamSpecDef,
        binding: Option<&mut manifold_core::effect_graph_def::BindingDef>,
    ) -> SpecReshapeSnapshot {
        let prev = SpecReshapeSnapshot {
            name: spec.name.clone(),
            min: spec.min,
            max: spec.max,
            invert: spec.invert,
            curve: spec.curve,
            scale: binding.as_ref().map(|b| b.scale),
            offset: binding.as_ref().map(|b| b.offset),
        };
        if let Some(label) = &self.label {
            spec.name = label.clone();
        }
        if let Some(min) = self.min {
            spec.min = min;
        }
        if let Some(max) = self.max {
            spec.max = max;
        }
        if let Some(invert) = self.invert {
            spec.invert = invert;
        }
        if let Some(curve) = self.curve {
            spec.curve = curve;
        }
        if let Some(b) = binding {
            if let Some(scale) = self.scale {
                b.scale = scale;
            }
            if let Some(offset) = self.offset {
                b.offset = offset;
            }
        }
        prev
    }
}

/// Pre-edit snapshot of a preset param's reshape, for undo.
#[derive(Debug, Clone)]
struct SpecReshapeSnapshot {
    name: String,
    min: f32,
    max: f32,
    invert: bool,
    curve: manifold_core::macro_bank::MacroCurve,
    scale: Option<f32>,
    offset: Option<f32>,
}

impl SpecReshapeSnapshot {
    fn restore(
        &self,
        spec: &mut manifold_core::effect_graph_def::ParamSpecDef,
        binding: Option<&mut manifold_core::effect_graph_def::BindingDef>,
    ) {
        spec.name = self.name.clone();
        spec.min = self.min;
        spec.max = self.max;
        spec.invert = self.invert;
        spec.curve = self.curve;
        if let Some(b) = binding {
            if let Some(scale) = self.scale {
                b.scale = scale;
            }
            if let Some(offset) = self.offset {
                b.offset = offset;
            }
        }
    }
}

/// Edit one card param's reshape — its display label, min/max range, invert
/// flag, response curve, or scale/offset — on an effect or generator addressed
/// by a [`GraphTarget`].
///
/// The reshape lives in the PRESET's authoring surface (its `ParamSpecDef` +
/// `BindingDef`), the single source after `ParamMapping` was deleted. The edit
/// targets the instance's per-instance graph override (`graph` for an effect,
/// `generator_graph` for a generator): if the instance is still on the catalog
/// default (`graph: None`), the caller-supplied `seed_def` (the catalog graph,
/// resolved renderer-side) materializes it first, so a recalibration becomes a
/// per-instance override exactly like a topology edit. The param is addressed by
/// its stable id (never mutated — drivers/Ableton/OSC reference it).
///
/// On first execute the pre-edit spec values are snapshotted for undo; the graph
/// version bump makes the renderer rebuild the binding (which reads its reshape
/// from the spec) next frame.
#[derive(Debug)]
pub struct EditParamMappingCommand {
    target: GraphTarget,
    binding_id: String,
    new: BindingMappingEdit,
    /// The catalog graph def used to materialize the instance's per-instance
    /// graph when it's still on the catalog default. Resolved renderer-side by
    /// the caller. `None` skips seeding (edits only an already-diverged graph).
    seed_def: Option<manifold_core::effect_graph_def::EffectGraphDef>,
    /// Explicit pre-drag reverse for the drag-commit path. Captured at drag
    /// start (before any live preview mutated the spec), so undo lands on the
    /// true pre-drag values rather than the preview-mutated ones. When set, it
    /// is applied on undo instead of the self-captured snapshot — mirroring
    /// `ChangeEffectParamCommand`'s explicit `old_value`.
    explicit_reverse: Option<BindingMappingEdit>,
    /// Pre-edit snapshot, captured on first execute for the single-shot path
    /// (when `explicit_reverse` is `None`).
    reverse: Option<SpecReshapeSnapshot>,
}

impl EditParamMappingCommand {
    pub fn new(
        target: GraphTarget,
        binding_id: String,
        new: BindingMappingEdit,
        seed_def: Option<manifold_core::effect_graph_def::EffectGraphDef>,
    ) -> Self {
        Self {
            target,
            binding_id,
            new,
            seed_def,
            explicit_reverse: None,
            reverse: None,
        }
    }

    /// Drag-commit variant: carries the EXPLICIT pre-drag reverse (a partial
    /// [`BindingMappingEdit`] of the same fields the drag moved), so undo
    /// restores the pre-drag values, not the values the live preview left in
    /// the spec.
    pub fn new_with_reverse(
        target: GraphTarget,
        binding_id: String,
        new: BindingMappingEdit,
        reverse: BindingMappingEdit,
        seed_def: Option<manifold_core::effect_graph_def::EffectGraphDef>,
    ) -> Self {
        Self {
            target,
            binding_id,
            new,
            seed_def,
            explicit_reverse: Some(reverse),
            reverse: None,
        }
    }
}

impl Command for EditParamMappingCommand {
    fn execute(&mut self, project: &mut Project) {
        let binding_id = self.binding_id.clone();
        let new = self.new.clone();
        let seed_def = self.seed_def.clone();
        // The drag-commit path supplies its own pre-drag reverse, so skip the
        // self-capture (which would snapshot the preview-mutated spec).
        let keep_snapshot = self.explicit_reverse.is_none();
        let snap = project
            .with_preset_graph_mut(&self.target, |host| {
                // Materialize the per-instance graph override if the instance is
                // still on the catalog default, so the reshape has a home.
                if host.graph_def().is_none() {
                    let seed = seed_def?;
                    *host.graph_def_mut() = Some(seed);
                    host.bump_graph_structure_version();
                }
                let graph = host.graph_def_mut().as_mut()?;
                let meta = graph.preset_metadata.as_mut()?;
                let spec = meta.params.iter_mut().find(|p| p.id == binding_id)?;
                let binding = meta.bindings.iter_mut().find(|b| b.id == binding_id);
                let snap = new.apply_to_spec(spec, binding);
                host.bump_graph_version();
                Some(snap)
            })
            .flatten();
        if keep_snapshot {
            self.reverse = snap;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        let binding_id = self.binding_id.clone();
        // Drag-commit: apply the explicit pre-drag reverse edit (partial — only
        // the fields the drag moved), landing on the true pre-drag values.
        if let Some(reverse_edit) = self.explicit_reverse.clone() {
            project.with_preset_graph_mut(&self.target, |host| {
                let Some(graph) = host.graph_def_mut().as_mut() else {
                    return;
                };
                let Some(meta) = graph.preset_metadata.as_mut() else {
                    return;
                };
                if let Some(spec) = meta.params.iter_mut().find(|p| p.id == binding_id) {
                    let binding = meta.bindings.iter_mut().find(|b| b.id == binding_id);
                    reverse_edit.apply_to_spec(spec, binding);
                }
                host.bump_graph_version();
            });
            return;
        }
        // Single-shot: restore the self-captured pre-edit snapshot.
        let Some(reverse) = self.reverse.clone() else {
            return;
        };
        project.with_preset_graph_mut(&self.target, |host| {
            let Some(graph) = host.graph_def_mut().as_mut() else {
                return;
            };
            let Some(meta) = graph.preset_metadata.as_mut() else {
                return;
            };
            if let Some(spec) = meta.params.iter_mut().find(|p| p.id == binding_id) {
                let binding = meta.bindings.iter_mut().find(|b| b.id == binding_id);
                reverse.restore(spec, binding);
            }
            host.bump_graph_version();
        });
    }

    fn description(&self) -> &str {
        "Edit param mapping"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::PresetTypeId;
    use manifold_core::macro_bank::MacroCurve;

    fn sample_user_binding(id: &str) -> UserParamBinding {
        UserParamBinding {
            id: id.to_string(),
            label: "Original Label".to_string(),
            node_id: manifold_core::NodeId::new("uv_transform"),
            legacy_node_handle: None,
            inner_param: "rotation".to_string(),
            min: 0.0,
            max: 1.0,
            default_value: 0.25,
            convert: manifold_core::effects::ParamConvert::Float,
            is_angle: false,
            invert: false,
            curve: MacroCurve::Linear,
            scale: 1.0,
            offset: 0.0,
        }
    }

    /// Build a project with one master effect carrying one user
    /// binding. Returns the project, the effect id, and the binding id.
    /// The binding lives in the effect's graph metadata (the single
    /// binding-storage list) via `append_user_binding`.
    fn project_with_one_user_binding() -> (Project, EffectId, String) {
        let mut project = Project::default();
        let mut fx = PresetInstance::new(PresetTypeId::new("Mirror"));
        let effect_id = fx.id.clone();
        let binding_id = "user.uv_transform.rotation.1".to_string();
        fx.append_user_binding(sample_user_binding(&binding_id));
        project.settings.master_effects.push(fx);
        (project, effect_id, binding_id)
    }

    /// The synthesized binding view for `id` — routing from the graph
    /// binding, reshape read from the per-instance `ParamSpecDef` +
    /// `BindingDef`. Owned because it's reconstructed.
    fn master_binding(project: &Project, id: &str) -> UserParamBinding {
        project.settings.master_effects[0]
            .user_param_bindings()
            .into_iter()
            .find(|b| b.id == id)
            .expect("binding present")
    }

    /// A minimal catalog graph def carrying one stock card param (`param_id`):
    /// a `ParamSpecDef` (range/curve/invert home) plus a `BindingDef`
    /// (scale/offset home) pointing at a `grade.amount` inner consumer. Used to
    /// seed the per-instance graph the reshape command materializes when an
    /// instance is still on the catalog default — the editing crate doesn't
    /// link the renderer, so it stands in for the renderer-resolved catalog def.
    fn seed_def_with_param(param_id: &str) -> manifold_core::effect_graph_def::EffectGraphDef {
        use manifold_core::effect_graph_def::{
            BindingDef, BindingTarget, EffectGraphDef, ParamSpecDef, PresetMetadata,
        };
        EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: Some(PresetMetadata {
                id: PresetTypeId::new("ColorGrade"),
                display_name: String::new(),
                category: String::new(),
                osc_prefix: String::new(),
                legacy_discriminant: None,
                available: true,
                is_line_based: false,
                params: vec![ParamSpecDef {
                    id: param_id.to_string(),
                    name: "Amount".to_string(),
                    min: 0.0,
                    max: 1.0,
                    default_value: 0.0,
                    whole_numbers: false,
                    is_toggle: false,
                    is_trigger: false,
                    value_labels: Vec::new(),
                    format_string: None,
                    osc_suffix: String::new(),
                    curve: MacroCurve::Linear,
                    invert: false,
                }],
                bindings: vec![BindingDef {
                    id: param_id.to_string(),
                    label: "Amount".to_string(),
                    default_value: 0.0,
                    target: BindingTarget::Node {
                        node_id: manifold_core::NodeId::new("grade"),
                        param: "amount".to_string(),
                    },
                    convert: manifold_core::effects::ParamConvert::Float,
                    user_added: false,
                    scale: 1.0,
                    offset: 0.0,
                }],
                skip_mode: Default::default(),
                param_aliases: Vec::new(),
                value_aliases: Vec::new(),
                string_params: Vec::new(),
                string_bindings: Vec::new(),
            }),
            nodes: Vec::new(),
            wires: Vec::new(),
        }
    }

    /// Read a stock card param's `(invert, max, scale)` straight out of an
    /// instance's per-instance graph spec — the single reshape home.
    fn spec_reshape(graph: &Option<manifold_core::effect_graph_def::EffectGraphDef>, id: &str) -> (bool, f32, f32) {
        let meta = graph
            .as_ref()
            .and_then(|g| g.preset_metadata.as_ref())
            .expect("graph carries preset metadata");
        let spec = meta.params.iter().find(|p| p.id == id).expect("param spec present");
        let scale = meta
            .bindings
            .iter()
            .find(|b| b.id == id)
            .map(|b| b.scale)
            .expect("binding present");
        (spec.invert, spec.max, scale)
    }

    #[test]
    fn edit_mapping_roundtrip_execute_undo_redo() {
        let (mut project, effect_id, binding_id) = project_with_one_user_binding();
        // The user binding already lives in the per-instance graph
        // (`append_user_binding` creates it), so no seed is needed. A reshape
        // edit bumps `graph_version` (the renderer rebuilds the binding list).
        let v0 = project.settings.master_effects[0].graph_version;

        let edit = BindingMappingEdit {
            label: Some("Spin".to_string()),
            min: Some(-2.0),
            max: Some(3.5),
            invert: Some(true),
            curve: Some(MacroCurve::SCurve),
            scale: Some(0.017453293),
            offset: Some(1.5),
        };
        let mut cmd =
            EditParamMappingCommand::new(GraphTarget::Effect(effect_id), binding_id.clone(), edit, None);

        // ── execute: every touched field changes ──
        cmd.execute(&mut project);
        let b = master_binding(&project, &binding_id);
        assert_eq!(b.id, binding_id, "id must NEVER change");
        assert_eq!(b.label, "Spin");
        assert_eq!(b.min, -2.0);
        assert_eq!(b.max, 3.5);
        assert!(b.invert);
        assert_eq!(b.curve, MacroCurve::SCurve);
        assert_eq!(b.scale, 0.017453293);
        assert_eq!(b.offset, 1.5);
        // Routing/identity fields untouched.
        assert_eq!(b.node_id, "uv_transform");
        assert_eq!(b.inner_param, "rotation");
        assert_eq!(b.default_value, 0.25);
        // Version bumped so the renderer rebuilds the reshaped binding.
        let v1 = project.settings.master_effects[0].graph_version;
        assert_ne!(v1, v0, "execute must bump graph_version");

        // ── undo: every touched field restored to its pre-edit value ──
        cmd.undo(&mut project);
        let b = master_binding(&project, &binding_id);
        assert_eq!(b.label, "Original Label");
        assert_eq!(b.min, 0.0);
        assert_eq!(b.max, 1.0);
        assert!(!b.invert);
        assert_eq!(b.curve, MacroCurve::Linear);
        assert_eq!(b.scale, 1.0, "scale restored to identity on undo");
        assert_eq!(b.offset, 0.0, "offset restored to identity on undo");
        let v2 = project.settings.master_effects[0].graph_version;
        assert_ne!(v2, v1, "undo must bump graph_version");

        // ── redo: reapplies cleanly ──
        cmd.execute(&mut project);
        let b = master_binding(&project, &binding_id);
        assert_eq!(b.label, "Spin");
        assert_eq!(b.min, -2.0);
        assert_eq!(b.max, 3.5);
        assert!(b.invert);
        assert_eq!(b.curve, MacroCurve::SCurve);
        assert_eq!(b.scale, 0.017453293);
        assert_eq!(b.offset, 1.5);
    }

    #[test]
    fn edit_mapping_only_touches_some_fields() {
        // A partial edit (only invert + curve) must leave label / min /
        // max untouched, and undo must restore exactly those two.
        let (mut project, effect_id, binding_id) = project_with_one_user_binding();
        let edit = BindingMappingEdit {
            invert: Some(true),
            curve: Some(MacroCurve::Exponential),
            ..Default::default()
        };
        let mut cmd =
            EditParamMappingCommand::new(GraphTarget::Effect(effect_id), binding_id.clone(), edit, None);
        cmd.execute(&mut project);
        let b = master_binding(&project, &binding_id);
        assert!(b.invert);
        assert_eq!(b.curve, MacroCurve::Exponential);
        // Untouched fields keep their original values.
        assert_eq!(b.label, "Original Label");
        assert_eq!(b.min, 0.0);
        assert_eq!(b.max, 1.0);

        cmd.undo(&mut project);
        let b = master_binding(&project, &binding_id);
        assert!(!b.invert);
        assert_eq!(b.curve, MacroCurve::Linear);
        assert_eq!(b.label, "Original Label");
    }

    #[test]
    fn edit_mapping_unknown_binding_is_noop() {
        // Addressing a binding id that doesn't exist must not panic and
        // must leave the effect untouched; undo with no captured reverse
        // is a clean no-op.
        let (mut project, effect_id, _binding_id) = project_with_one_user_binding();
        let v0 = project.settings.master_effects[0].graph_version;
        let mut cmd = EditParamMappingCommand::new(
            GraphTarget::Effect(effect_id),
            "user.does.not.exist.1".to_string(),
            BindingMappingEdit {
                invert: Some(true),
                ..Default::default()
            },
            None,
        );
        cmd.execute(&mut project);
        assert!(cmd.reverse.is_none(), "no reverse captured for a no-op");
        let v1 = project.settings.master_effects[0].graph_version;
        assert_eq!(v1, v0, "no-op must not bump the version");
        // Undo is a clean no-op.
        cmd.undo(&mut project);
        assert_eq!(project.settings.master_effects[0].graph_version, v0);
    }

    /// A STOCK param (no user binding) on an instance still at the catalog
    /// default (`graph: None`) materializes the per-instance graph from the
    /// supplied `seed_def`, then edits the reshape on that graph's spec — the
    /// post-`ParamMapping` model. Undo restores the seeded spec values.
    #[test]
    fn edit_stock_param_seeds_graph_and_roundtrips() {
        let mut project = Project::default();
        let fx = PresetInstance::new(PresetTypeId::new("ColorGrade"));
        let effect_id = fx.id.clone();
        project.settings.master_effects.push(fx);
        assert!(
            project.settings.master_effects[0].graph.is_none(),
            "instance starts on the catalog default (no per-instance graph)",
        );

        let edit = BindingMappingEdit {
            invert: Some(true),
            scale: Some(2.0),
            max: Some(5.0),
            ..Default::default()
        };
        let v0 = project.settings.master_effects[0].graph_version;
        let mut cmd = EditParamMappingCommand::new(
            GraphTarget::Effect(effect_id),
            "amount".to_string(),
            edit,
            Some(seed_def_with_param("amount")),
        );

        // execute: materializes the graph + applies the reshape to its spec.
        cmd.execute(&mut project);
        assert!(
            project.settings.master_effects[0].graph.is_some(),
            "editing a stock param materializes the per-instance graph",
        );
        assert_eq!(
            spec_reshape(&project.settings.master_effects[0].graph, "amount"),
            (true, 5.0, 2.0),
            "reshape lands on the materialized graph's spec + binding",
        );
        assert_ne!(
            project.settings.master_effects[0].graph_version, v0,
            "editing a stock param bumps graph_version",
        );

        // undo: the seeded spec values are restored.
        cmd.undo(&mut project);
        assert_eq!(
            spec_reshape(&project.settings.master_effects[0].graph, "amount"),
            (false, 1.0, 1.0),
            "undo restores the seeded spec (invert false, max 1.0, scale 1.0)",
        );
    }

    /// An id that is neither a user binding nor present in the seed def, with
    /// no seed supplied, must be a clean no-op — no graph materialized, no
    /// reverse captured.
    #[test]
    fn edit_unknown_stock_id_without_seed_is_noop() {
        let mut project = Project::default();
        let fx = PresetInstance::new(PresetTypeId::new("ColorGrade"));
        let effect_id = fx.id.clone();
        project.settings.master_effects.push(fx);

        let mut cmd = EditParamMappingCommand::new(
            GraphTarget::Effect(effect_id),
            "totally.unknown.param".to_string(),
            BindingMappingEdit {
                invert: Some(true),
                ..Default::default()
            },
            None,
        );
        cmd.execute(&mut project);
        assert!(cmd.reverse.is_none(), "unknown id without a seed must be a no-op");
        assert!(
            project.settings.master_effects[0].graph.is_none(),
            "no per-instance graph materialized for an unknown id without a seed",
        );
    }

    /// Generator command: materialize the layer's `generator_graph` from the
    /// seed def, edit the reshape on its spec, undo restores the seeded values.
    /// The generator host edits `Layer::generator_graph`, mirroring the effect
    /// path's per-instance `graph`.
    #[test]
    fn edit_gen_param_seeds_graph_and_roundtrips() {
        use manifold_core::PresetTypeId;
        use manifold_core::layer::Layer;

        let mut project = Project::default();
        let layer = Layer::new_generator("Gen".into(), PresetTypeId::new("Plasma"), 0);
        let layer_id = layer.layer_id.clone();
        project.timeline.insert_layer(0, layer);

        let edit = BindingMappingEdit {
            invert: Some(true),
            scale: Some(2.0),
            ..Default::default()
        };
        let mut cmd = EditParamMappingCommand::new(
            GraphTarget::Generator(layer_id.clone()),
            "complexity".to_string(),
            edit,
            Some(seed_def_with_param("complexity")),
        );

        let reshape = |p: &Project, lid: &str| -> (bool, f32) {
            let layer = p
                .timeline
                .layers
                .iter()
                .find(|l| l.layer_id.as_str() == lid)
                .expect("layer present");
            // The generator graph lives on gen_params now (graph-home unification).
            let gp = layer.gen_params().expect("gen params present");
            let (invert, _max, scale) = spec_reshape(&gp.graph, "complexity");
            (invert, scale)
        };

        // execute: materializes generator_graph + applies the reshape.
        cmd.execute(&mut project);
        assert_eq!(reshape(&project, layer_id.as_str()), (true, 2.0));

        // undo restores the seeded spec values.
        cmd.undo(&mut project);
        assert_eq!(reshape(&project, layer_id.as_str()), (false, 1.0));

        // redo re-applies.
        cmd.execute(&mut project);
        assert_eq!(reshape(&project, layer_id.as_str()), (true, 2.0));
    }

    /// The drag-undo fix: a live-drag preview mutates the spec to the final
    /// value BEFORE commit, so a self-capturing command would record the wrong
    /// "before." `new_with_reverse` carries the explicit pre-drag value (from
    /// the drag-start snapshot), so undo restores it — not the preview-mutated
    /// final. Proven for a user binding (the same explicit path serves stock +
    /// generator graphs).
    #[test]
    fn drag_commit_explicit_reverse_restores_pre_drag_not_preview_final() {
        let (mut project, effect_id, binding_id) = project_with_one_user_binding();
        // Simulate the live-drag preview having already moved scale 1.0 -> 3.0.
        // The reshape lives on the per-instance graph's BindingDef now, so mutate
        // it directly (what `preview_mapping` does via the discarded command).
        {
            let meta = project.settings.master_effects[0]
                .graph
                .as_mut()
                .and_then(|g| g.preset_metadata.as_mut())
                .expect("user binding created the per-instance graph");
            meta.bindings
                .iter_mut()
                .find(|b| b.id == binding_id)
                .expect("binding present")
                .scale = 3.0;
        }

        let mut cmd = EditParamMappingCommand::new_with_reverse(
            GraphTarget::Effect(effect_id),
            binding_id.clone(),
            BindingMappingEdit {
                scale: Some(3.0), // new = the dragged-to (final) value
                ..Default::default()
            },
            BindingMappingEdit {
                scale: Some(1.0), // explicit pre-drag reverse from the snapshot
                ..Default::default()
            },
            None,
        );
        cmd.execute(&mut project);
        assert_eq!(master_binding(&project, &binding_id).scale, 3.0, "commit lands final");

        cmd.undo(&mut project);
        assert_eq!(
            master_binding(&project, &binding_id).scale, 1.0,
            "undo restores the PRE-DRAG value (1.0), not the preview-mutated final (3.0) \
             — the quirk Peter caught",
        );

        // redo re-applies the final.
        cmd.execute(&mut project);
        assert_eq!(master_binding(&project, &binding_id).scale, 3.0, "redo re-lands final");
    }
}
