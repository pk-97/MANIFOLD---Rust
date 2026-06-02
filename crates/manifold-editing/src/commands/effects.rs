use crate::command::Command;
use crate::commands::effect_target::{EffectTarget, with_effects_mut};
use manifold_core::{EffectId, LayerId};
use manifold_core::effects::{
    EffectInstance, ParamConvert, ParamEnvelope, ParamId, ParamMapping, ParamSlot, ParameterDriver,
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
    effect: EffectInstance,
    insert_index: usize,
}

impl AddEffectCommand {
    pub fn new(target: EffectTarget, effect: EffectInstance, insert_index: usize) -> Self {
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
    effect: Option<EffectInstance>,
    removed_index: usize,
}

impl RemoveEffectCommand {
    pub fn new(target: EffectTarget, effect: EffectInstance, removed_index: usize) -> Self {
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
    old_effects: Vec<EffectInstance>,
    /// Snapshot of the entire effects vec after the reorder.
    new_effects: Vec<EffectInstance>,
}

impl ReorderEffectGroupCommand {
    /// Construct from before/after snapshots of the effects vec.
    pub fn new(
        target: EffectTarget,
        old_effects: Vec<EffectInstance>,
        new_effects: Vec<EffectInstance>,
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

/// Change a single parameter value on an effect.
///
/// Addresses the parameter by stable [`ParamId`] (not by position).
/// The id is resolved against the effect registry on each
/// `execute`/`undo` so an undo entry stays correct even if the
/// effect's param list is reordered between recording and replaying.
///
/// Step 16 of `docs/EFFECT_RUNTIME_UNIFICATION.md` §11.
#[derive(Debug)]
pub struct ChangeEffectParamCommand {
    effect_id: EffectId,
    param_id: ParamId,
    old_value: f32,
    new_value: f32,
}

impl ChangeEffectParamCommand {
    pub fn new(
        effect_id: EffectId,
        param_id: impl Into<ParamId>,
        old_value: f32,
        new_value: f32,
    ) -> Self {
        Self {
            effect_id,
            param_id: param_id.into(),
            old_value,
            new_value,
        }
    }
}

impl Command for ChangeEffectParamCommand {
    fn execute(&mut self, project: &mut Project) {
        let id = self.param_id.clone();
        let val = self.new_value;
        if let Some(effect) = project.find_effect_by_id_mut(&self.effect_id)
            && let Some(idx) = effect.param_id_to_value_index(id.as_ref())
        {
            effect.set_base_param(idx, val);
        }
    }

    fn undo(&mut self, project: &mut Project) {
        let id = self.param_id.clone();
        let val = self.old_value;
        if let Some(effect) = project.find_effect_by_id_mut(&self.effect_id)
            && let Some(idx) = effect.param_id_to_value_index(id.as_ref())
        {
            effect.set_base_param(idx, val);
        }
    }

    fn description(&self) -> &str {
        "Change Effect Param"
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
    existing: &[UserParamBinding],
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
        if !existing.iter().any(|b| b.id == candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Toggle whether an inner-graph parameter is user-exposed on an
/// [`EffectInstance`]. One command for both directions (expose /
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
    /// Identifies the inner-graph addressing being toggled. The
    /// command keys both directions (Expose creates a binding here;
    /// Unexpose removes the binding matching this addressing).
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
        /// Drivers pruned from `EffectInstance.drivers` because their
        /// `param_id` matched the removed binding's id.
        removed_drivers: Vec<ParameterDriver>,
        /// Ableton mappings pruned from
        /// `EffectInstance.ableton_mappings` for the same reason.
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
        node_handle: String,
        inner_param: String,
        expose: bool,
        inner_meta: InnerParamMeta,
    ) -> Self {
        Self {
            effect_id,
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
        let node_handle = self.node_handle.clone();
        let inner_param = self.inner_param.clone();
        let expose = self.expose;
        let meta = self.inner_meta.clone();
        // Resolve the instance by id (master / layer / clip). `None` when the
        // id doesn't resolve — leaves `reverse` as `None`, a clean no-op.
        let reverse_out = project.find_effect_by_id_mut(&self.effect_id).map(|effect| {
            // Locate any existing binding for this (handle, inner_param).
            let existing_position = effect
                .user_param_bindings
                .iter()
                .position(|b| b.node_handle == node_handle && b.inner_param == inner_param);

            if expose {
                // Idempotent: if already exposed, no-op.
                if existing_position.is_some() {
                    return ReverseState::None;
                }
                let id =
                    generate_user_param_id(&node_handle, &inner_param, &effect.user_param_bindings);
                let binding = UserParamBinding {
                    id: id.clone(),
                    label: meta.label.clone(),
                    node_handle: node_handle.clone(),
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
                let user_param_id = effect.user_param_bindings[position].id.clone();
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
                    // user_param_id by string. position is bounded by
                    // the current vec len, clamp defensively.
                    let pos = position.min(effect.user_param_bindings.len());
                    let binding_id = binding.id.clone();
                    effect.user_param_bindings.insert(pos, binding);
                    effect.user_param_bindings_version =
                        effect.user_param_bindings_version.wrapping_add(1);
                    // Restore the param_values slot at the corresponding
                    // value-index. With user_param_bindings updated, the
                    // value index is now resolvable via the helper.
                    if let Some(value_idx) = effect.param_id_to_value_index(&binding_id) {
                        if value_idx <= effect.param_values.len() {
                            effect.param_values.insert(value_idx, slot_value);
                        } else {
                            effect.param_values.push(slot_value);
                        }
                        if let (Some(base), Some(base_v)) =
                            (effect.base_param_values.as_mut(), slot_base_value)
                            && value_idx <= base.len()
                        {
                            base.insert(value_idx, base_v);
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
/// directions by [`EditUserParamBindingCommand`] — `new` carries the
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
    /// Apply each `Some` field onto `binding`. `None` fields are
    /// skipped, so an edit only mutates what the user touched.
    fn apply_to(&self, binding: &mut UserParamBinding) {
        if let Some(label) = &self.label {
            binding.label = label.clone();
        }
        if let Some(min) = self.min {
            binding.min = min;
        }
        if let Some(max) = self.max {
            binding.max = max;
        }
        if let Some(invert) = self.invert {
            binding.invert = invert;
        }
        if let Some(curve) = self.curve {
            binding.curve = curve;
        }
        if let Some(scale) = self.scale {
            binding.scale = scale;
        }
        if let Some(offset) = self.offset {
            binding.offset = offset;
        }
    }

    /// Snapshot the binding's *current* values for exactly the fields
    /// `self` touches, so an undo can restore them. Returns a
    /// `BindingMappingEdit` with `Some` set for the same fields and
    /// `None` everywhere else.
    fn snapshot_inverse(&self, binding: &UserParamBinding) -> BindingMappingEdit {
        BindingMappingEdit {
            label: self.label.as_ref().map(|_| binding.label.clone()),
            min: self.min.map(|_| binding.min),
            max: self.max.map(|_| binding.max),
            invert: self.invert.map(|_| binding.invert),
            curve: self.curve.map(|_| binding.curve),
            scale: self.scale.map(|_| binding.scale),
            offset: self.offset.map(|_| binding.offset),
        }
    }

    /// Apply each touched field onto a stock-param reshape note
    /// ([`ParamMapping`]). Same partial-edit semantics as
    /// [`Self::apply_to`] but for the note shape (whose `label` is an
    /// `Option<String>` override over the recipe label).
    fn apply_to_mapping(&self, m: &mut ParamMapping) {
        if let Some(label) = &self.label {
            m.label = Some(label.clone());
        }
        if let Some(min) = self.min {
            m.min = min;
        }
        if let Some(max) = self.max {
            m.max = max;
        }
        if let Some(invert) = self.invert {
            m.invert = invert;
        }
        if let Some(curve) = self.curve {
            m.curve = curve;
        }
        if let Some(scale) = self.scale {
            m.scale = scale;
        }
        if let Some(offset) = self.offset {
            m.offset = offset;
        }
    }
}

/// Seed a fresh reshape note for a STOCK param, identity-valued so an
/// un-edited note is byte-identical to the recipe. Range comes from the
/// registry `ParamDef` when available (production always has it); a
/// `0..1` fallback covers registry-less unit-test contexts and only
/// matters for invert/curve (scale/offset reshapes ignore the range).
fn seed_stock_note(effect: &EffectInstance, param_id: &str) -> ParamMapping {
    let (min, max) = manifold_core::effect_definition_registry::try_get(effect.effect_type())
        .and_then(|def| def.id_to_index.get(param_id).map(|&i| (def, i)))
        .map(|(def, i)| (def.param_defs[i].min, def.param_defs[i].max))
        .unwrap_or((0.0, 1.0));
    ParamMapping {
        param_id: param_id.to_string(),
        label: None,
        min,
        max,
        invert: false,
        curve: manifold_core::macro_bank::MacroCurve::Linear,
        scale: 1.0,
        offset: 0.0,
    }
}

/// Undo state for [`EditUserParamBindingCommand`]. The reshape can live
/// in either of the two per-instance stores, so undo has to know which
/// one and how to revert it.
#[derive(Debug, Clone)]
enum MappingReverse {
    /// User-exposed binding (inline reshape): restore exactly the touched
    /// fields.
    UserFields(BindingMappingEdit),
    /// Stock-param note that already existed: restore the whole prior
    /// note (cheaper + simpler than field-wise, and a note is 7 fields).
    NoteRestore(ParamMapping),
    /// Stock-param note this command CREATED (copy-on-write): undo removes
    /// it, returning the knob to the recipe's reshape.
    NoteRemove(String),
    /// Note path with an EXPLICIT pre-drag reverse (the drag-commit case):
    /// undo applies these field values to the note. Used when a live-drag
    /// preview already mutated the note before commit, so the command can't
    /// self-capture the true pre-drag value — the caller supplies it from
    /// the drag-start snapshot, mirroring `ChangeEffectParamCommand`'s
    /// explicit `old_value`.
    NoteApply(BindingMappingEdit),
}

/// Edit a [`UserParamBinding`]'s card-slider mapping — its display
/// label, min/max range, invert flag, or response curve. The binding is
/// addressed by its stable [`UserParamBinding::id`], which this command
/// NEVER mutates: drivers, Ableton mappings, envelopes, and OSC paths
/// all reference that id by string, so renaming it would orphan every
/// modulation surface the user configured.
///
/// On first execute the pre-edit values of the touched fields are
/// snapshotted into [`Self::reverse`] so undo can restore them. Both
/// directions bump `user_param_bindings_version`, which makes the
/// renderer rebuild this effect's user-binding tail on the next frame:
/// each `ResolvedBinding` is re-derived via `ResolvedBinding::from_user`
/// (recomputing its `reshape` from the freshly-edited invert/curve), and
/// the per-binding `LastAppliedCache` user-tail is dropped via
/// `clear_tail`, so the new mapping takes effect immediately rather than
/// waiting for the slot value to change.
///
/// Mirrors [`ToggleEffectParamExposeCommand`] field-for-field on
/// addressing (`effect_id` + `find_effect_by_id_mut`).
#[derive(Debug)]
pub struct EditUserParamBindingCommand {
    effect_id: EffectId,
    /// Stable id of the binding being edited. NEVER mutated.
    binding_id: String,
    /// Post-edit values for the touched fields.
    new: BindingMappingEdit,
    /// How to undo, captured on first execute(). `None` until then (and
    /// stays `None` if the effect doesn't resolve — a no-op). Carries
    /// which store the reshape lives in (user binding vs stock note) so
    /// undo reverts the right one.
    reverse: Option<MappingReverse>,
    /// Explicit pre-drag reverse for the drag-commit case. When set, undo
    /// restores THESE field values instead of whatever the command would
    /// self-capture at execute time — necessary because a live-drag
    /// preview already mutated the store before commit. `None` for
    /// single-shot edits (invert / curve / label), which self-capture
    /// correctly since nothing mutated the store first.
    explicit_reverse: Option<BindingMappingEdit>,
}

impl EditUserParamBindingCommand {
    pub fn new(effect_id: EffectId, binding_id: String, new: BindingMappingEdit) -> Self {
        Self {
            effect_id,
            binding_id,
            new,
            reverse: None,
            explicit_reverse: None,
        }
    }

    /// Drag-commit constructor: `reverse` carries the pre-drag field values
    /// captured at drag start, so undo restores them (not the
    /// preview-mutated values the command would otherwise self-capture).
    pub fn new_with_reverse(
        effect_id: EffectId,
        binding_id: String,
        new: BindingMappingEdit,
        reverse: BindingMappingEdit,
    ) -> Self {
        Self {
            effect_id,
            binding_id,
            new,
            reverse: None,
            explicit_reverse: Some(reverse),
        }
    }
}

impl Command for EditUserParamBindingCommand {
    fn execute(&mut self, project: &mut Project) {
        let binding_id = self.binding_id.clone();
        let new = self.new.clone();
        let explicit = self.explicit_reverse.clone();
        self.reverse = project.find_effect_by_id_mut(&self.effect_id).and_then(|effect| {
            // User-exposed binding (inline reshape) — the existing path.
            if let Some(binding) = effect
                .user_param_bindings
                .iter_mut()
                .find(|b| b.id == binding_id)
            {
                // Explicit pre-drag reverse wins (drag commit); else snapshot
                // the OLD values of the touched fields BEFORE applying.
                let reverse = explicit
                    .clone()
                    .unwrap_or_else(|| new.snapshot_inverse(binding));
                new.apply_to(binding);
                // Bump so the renderer rebuilds the user-binding tail (and
                // drops its LastAppliedCache tail) on the next frame.
                effect.user_param_bindings_version =
                    effect.user_param_bindings_version.wrapping_add(1);
                return Some(MappingReverse::UserFields(reverse));
            }
            // Otherwise it's a STOCK param → a reshape note. Edit the
            // existing note, or seed one copy-on-write (identity, so the
            // un-edited note is byte-identical) and apply onto that.
            match effect.param_mapping(&binding_id).cloned() {
                Some(prior) => {
                    let mut note = prior.clone();
                    new.apply_to_mapping(&mut note);
                    effect.upsert_param_mapping(note); // bumps version
                    // Explicit reverse (drag) restores the pre-drag fields;
                    // else restore the whole prior note (single-shot).
                    Some(match explicit {
                        Some(e) => MappingReverse::NoteApply(e),
                        None => MappingReverse::NoteRestore(prior),
                    })
                }
                None => {
                    // Only seed a note for a REAL param (one that resolves
                    // to a value slot). A genuinely unknown id is a clean
                    // no-op — never mint an orphan note.
                    effect.param_id_to_value_index(&binding_id)?;
                    let mut note = seed_stock_note(effect, &binding_id);
                    new.apply_to_mapping(&mut note);
                    effect.upsert_param_mapping(note); // bumps version
                    // Single-shot removes the freshly-created note on undo;
                    // a drag restores the pre-drag (seed/identity) fields.
                    Some(match explicit {
                        Some(e) => MappingReverse::NoteApply(e),
                        None => MappingReverse::NoteRemove(binding_id),
                    })
                }
            }
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(reverse) = self.reverse.clone() else {
            return;
        };
        let binding_id = self.binding_id.clone();
        let Some(effect) = project.find_effect_by_id_mut(&self.effect_id) else {
            return;
        };
        match reverse {
            MappingReverse::UserFields(fields) => {
                if let Some(binding) = effect
                    .user_param_bindings
                    .iter_mut()
                    .find(|b| b.id == binding_id)
                {
                    fields.apply_to(binding);
                    effect.user_param_bindings_version =
                        effect.user_param_bindings_version.wrapping_add(1);
                }
            }
            MappingReverse::NoteRestore(prior) => {
                effect.upsert_param_mapping(prior); // bumps version
            }
            MappingReverse::NoteRemove(id) => {
                effect.remove_param_mapping(&id); // bumps version
            }
            MappingReverse::NoteApply(fields) => {
                if let Some(mut note) = effect.param_mapping(&binding_id).cloned() {
                    fields.apply_to_mapping(&mut note);
                    effect.upsert_param_mapping(note); // bumps version
                }
            }
        }
    }

    fn description(&self) -> &str {
        "Edit param mapping"
    }
}

/// Seed a fresh reshape note for a generator param, identity-valued.
/// Range from the generator registry `ParamDef` when available; a `0..1`
/// fallback covers JSON-only generators with no registry entry and
/// registry-less tests (only matters for invert/curve).
fn seed_gen_note(
    gp: &manifold_core::generator::GeneratorParamState,
    param_id: &str,
) -> ParamMapping {
    let (min, max) = manifold_core::generator_definition_registry::try_get(gp.generator_type())
        .and_then(|def| def.id_to_index.get(param_id).map(|&i| (def, i)))
        .map(|(def, i)| (def.param_defs[i].min, def.param_defs[i].max))
        .unwrap_or((0.0, 1.0));
    ParamMapping {
        param_id: param_id.to_string(),
        label: None,
        min,
        max,
        invert: false,
        curve: manifold_core::macro_bank::MacroCurve::Linear,
        scale: 1.0,
        offset: 0.0,
    }
}

/// Edit a generator card param's reshape note — the generator twin of
/// [`EditUserParamBindingCommand`]'s stock-param path. Generators have no
/// user-binding tier, so this is purely the note store
/// (`GeneratorParamState.param_mappings`), addressed by `layer_id` +
/// stable `param_id` (never mutated). Seeds the note copy-on-write
/// (identity, so the un-edited note is byte-identical to the recipe);
/// undo restores the prior note or removes a freshly-created one.
#[derive(Debug)]
pub struct EditGenParamMappingCommand {
    layer_id: LayerId,
    param_id: String,
    new: BindingMappingEdit,
    reverse: Option<MappingReverse>,
    /// Explicit pre-drag reverse for the drag-commit case — see
    /// [`EditUserParamBindingCommand::explicit_reverse`].
    explicit_reverse: Option<BindingMappingEdit>,
}

impl EditGenParamMappingCommand {
    pub fn new(layer_id: LayerId, param_id: String, new: BindingMappingEdit) -> Self {
        Self {
            layer_id,
            param_id,
            new,
            reverse: None,
            explicit_reverse: None,
        }
    }

    /// Drag-commit constructor — `reverse` carries the pre-drag fields.
    pub fn new_with_reverse(
        layer_id: LayerId,
        param_id: String,
        new: BindingMappingEdit,
        reverse: BindingMappingEdit,
    ) -> Self {
        Self {
            layer_id,
            param_id,
            new,
            reverse: None,
            explicit_reverse: Some(reverse),
        }
    }
}

impl Command for EditGenParamMappingCommand {
    fn execute(&mut self, project: &mut Project) {
        let param_id = self.param_id.clone();
        let new = self.new.clone();
        let explicit = self.explicit_reverse.clone();
        self.reverse = project
            .timeline
            .find_layer_by_id_mut(self.layer_id.as_str())
            .and_then(|(_, l)| l.gen_params_mut())
            .map(|gp| match gp.param_mapping(&param_id).cloned() {
                Some(prior) => {
                    let mut note = prior.clone();
                    new.apply_to_mapping(&mut note);
                    gp.upsert_param_mapping(note);
                    match explicit {
                        Some(e) => MappingReverse::NoteApply(e),
                        None => MappingReverse::NoteRestore(prior),
                    }
                }
                None => {
                    let mut note = seed_gen_note(gp, &param_id);
                    new.apply_to_mapping(&mut note);
                    gp.upsert_param_mapping(note);
                    match explicit {
                        Some(e) => MappingReverse::NoteApply(e),
                        None => MappingReverse::NoteRemove(param_id.clone()),
                    }
                }
            });
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(reverse) = self.reverse.clone() else {
            return;
        };
        let param_id = self.param_id.clone();
        let Some(gp) = project
            .timeline
            .find_layer_by_id_mut(self.layer_id.as_str())
            .and_then(|(_, l)| l.gen_params_mut())
        else {
            return;
        };
        match reverse {
            MappingReverse::NoteRestore(prior) => gp.upsert_param_mapping(prior),
            MappingReverse::NoteRemove(id) => gp.remove_param_mapping(&id),
            MappingReverse::NoteApply(fields) => {
                if let Some(mut note) = gp.param_mapping(&param_id).cloned() {
                    fields.apply_to_mapping(&mut note);
                    gp.upsert_param_mapping(note);
                }
            }
            // Generators have no user-binding tier; this variant is never
            // produced by this command.
            MappingReverse::UserFields(_) => {}
        }
    }

    fn description(&self) -> &str {
        "Edit generator param mapping"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::EffectTypeId;
    use manifold_core::macro_bank::MacroCurve;

    fn sample_user_binding(id: &str) -> UserParamBinding {
        UserParamBinding {
            id: id.to_string(),
            label: "Original Label".to_string(),
            node_handle: "uv_transform".to_string(),
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
    fn project_with_one_user_binding() -> (Project, EffectId, String) {
        let mut project = Project::default();
        let mut fx = EffectInstance::new(EffectTypeId::new("Mirror"));
        let effect_id = fx.id.clone();
        let binding_id = "user.uv_transform.rotation.1".to_string();
        fx.user_param_bindings.push(sample_user_binding(&binding_id));
        project.settings.master_effects.push(fx);
        (project, effect_id, binding_id)
    }

    fn master_binding<'a>(project: &'a Project, id: &str) -> &'a UserParamBinding {
        project.settings.master_effects[0]
            .user_param_bindings
            .iter()
            .find(|b| b.id == id)
            .expect("binding present")
    }

    #[test]
    fn edit_mapping_roundtrip_execute_undo_redo() {
        let (mut project, effect_id, binding_id) = project_with_one_user_binding();
        let v0 = project.settings.master_effects[0].user_param_bindings_version;

        let edit = BindingMappingEdit {
            label: Some("Spin".to_string()),
            min: Some(-2.0),
            max: Some(3.5),
            invert: Some(true),
            curve: Some(MacroCurve::SCurve),
            scale: Some(0.017453293),
            offset: Some(1.5),
        };
        let mut cmd = EditUserParamBindingCommand::new(effect_id, binding_id.clone(), edit);

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
        assert_eq!(b.node_handle, "uv_transform");
        assert_eq!(b.inner_param, "rotation");
        assert_eq!(b.default_value, 0.25);
        // Version bumped so the renderer rebuilds the user-binding tail.
        let v1 = project.settings.master_effects[0].user_param_bindings_version;
        assert_ne!(v1, v0, "execute must bump user_param_bindings_version");

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
        let v2 = project.settings.master_effects[0].user_param_bindings_version;
        assert_ne!(v2, v1, "undo must bump user_param_bindings_version");

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
        let mut cmd = EditUserParamBindingCommand::new(effect_id, binding_id.clone(), edit);
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
        let v0 = project.settings.master_effects[0].user_param_bindings_version;
        let mut cmd = EditUserParamBindingCommand::new(
            effect_id,
            "user.does.not.exist.1".to_string(),
            BindingMappingEdit {
                invert: Some(true),
                ..Default::default()
            },
        );
        cmd.execute(&mut project);
        assert!(cmd.reverse.is_none(), "no reverse captured for a no-op");
        let v1 = project.settings.master_effects[0].user_param_bindings_version;
        assert_eq!(v1, v0, "no-op must not bump the version");
        // Undo is a clean no-op.
        cmd.undo(&mut project);
        assert_eq!(
            project.settings.master_effects[0].user_param_bindings_version,
            v0
        );
    }

    /// A STOCK param (no user binding) routes to a per-instance reshape
    /// NOTE. Pre-seed a note so the path runs registry-free (the editing
    /// crate doesn't link the renderer, so the effect registry is empty —
    /// new-note creation is covered by the renderer/app integration gate).
    #[test]
    fn edit_stock_param_note_roundtrip() {
        let mut project = Project::default();
        let mut fx = EffectInstance::new(EffectTypeId::new("ColorGrade"));
        let effect_id = fx.id.clone();
        // Pre-existing identity note on stock param "amount".
        fx.upsert_param_mapping(ParamMapping {
            param_id: "amount".to_string(),
            label: None,
            min: 0.0,
            max: 1.0,
            invert: false,
            curve: MacroCurve::Linear,
            scale: 1.0,
            offset: 0.0,
        });
        project.settings.master_effects.push(fx);

        let edit = BindingMappingEdit {
            invert: Some(true),
            scale: Some(2.0),
            ..Default::default()
        };
        let mut cmd =
            EditUserParamBindingCommand::new(effect_id, "amount".to_string(), edit);

        // execute: edits the NOTE, not a user binding; bumps the note
        // version (not the user-binding version).
        let mv0 = project.settings.master_effects[0].param_mappings_version;
        cmd.execute(&mut project);
        let note = project.settings.master_effects[0]
            .param_mapping("amount")
            .expect("note still present after edit");
        assert!(note.invert, "stock note invert applied");
        assert_eq!(note.scale, 2.0, "stock note scale applied");
        assert_ne!(
            project.settings.master_effects[0].param_mappings_version, mv0,
            "editing a stock note bumps param_mappings_version",
        );

        // undo: whole prior note restored (NoteRestore).
        cmd.undo(&mut project);
        let note = project.settings.master_effects[0]
            .param_mapping("amount")
            .expect("note restored, not removed (it pre-existed)");
        assert!(!note.invert, "undo restores invert");
        assert_eq!(note.scale, 1.0, "undo restores scale to identity");
    }

    /// An id that is neither a user binding nor a resolvable param must
    /// NOT mint an orphan note (the no-op guard).
    #[test]
    fn edit_unknown_stock_id_does_not_create_note() {
        let mut project = Project::default();
        let fx = EffectInstance::new(EffectTypeId::new("ColorGrade"));
        let effect_id = fx.id.clone();
        project.settings.master_effects.push(fx);

        let mut cmd = EditUserParamBindingCommand::new(
            effect_id,
            "totally.unknown.param".to_string(),
            BindingMappingEdit {
                invert: Some(true),
                ..Default::default()
            },
        );
        cmd.execute(&mut project);
        assert!(cmd.reverse.is_none(), "unknown id must be a no-op");
        assert!(
            project.settings.master_effects[0].param_mappings.is_empty(),
            "no orphan note minted for an unknown id",
        );
    }

    /// Generator command: create a note copy-on-write, edit it, undo
    /// (NoteRemove since it was created), redo, then re-edit (NoteRestore).
    #[test]
    fn edit_gen_param_mapping_roundtrip() {
        use manifold_core::GeneratorTypeId;
        use manifold_core::layer::Layer;

        let mut project = Project::default();
        let layer = Layer::new_generator("Gen".into(), GeneratorTypeId::new("Plasma"), 0);
        let layer_id = layer.layer_id.clone();
        project.timeline.insert_layer(0, layer);
        assert!(
            project
                .timeline
                .find_layer_by_id_mut(layer_id.as_str())
                .and_then(|(_, l)| l.gen_params_mut())
                .is_some(),
            "generator layer has gen_params",
        );

        let edit = BindingMappingEdit {
            invert: Some(true),
            scale: Some(2.0),
            ..Default::default()
        };
        let mut cmd =
            EditGenParamMappingCommand::new(layer_id.clone(), "complexity".to_string(), edit);

        let note = |p: &Project, lid: &str| -> Option<ParamMapping> {
            p.timeline
                .layers
                .iter()
                .find(|l| l.layer_id.as_str() == lid)
                .and_then(|l| l.gen_params())
                .and_then(|gp| gp.param_mapping("complexity").cloned())
        };

        // create
        cmd.execute(&mut project);
        let n = note(&project, layer_id.as_str()).expect("note created");
        assert!(n.invert);
        assert_eq!(n.scale, 2.0);

        // undo removes the freshly-created note
        cmd.undo(&mut project);
        assert!(note(&project, layer_id.as_str()).is_none(), "undo removes created note");

        // redo re-creates
        cmd.execute(&mut project);
        assert!(note(&project, layer_id.as_str()).unwrap().invert);
    }

    /// The drag-undo fix: a live-drag preview mutates the store to the
    /// final value BEFORE commit, so a self-capturing command would record
    /// the wrong "before." `new_with_reverse` carries the explicit pre-drag
    /// value (from the drag-start snapshot), so undo restores it — not the
    /// preview-mutated final. Proven for a user binding (the same explicit
    /// path serves stock + generator notes).
    #[test]
    fn drag_commit_explicit_reverse_restores_pre_drag_not_preview_final() {
        let (mut project, effect_id, binding_id) = project_with_one_user_binding();
        // Simulate the live-drag preview having already moved scale 1.0 -> 3.0.
        project.settings.master_effects[0].user_param_bindings[0].scale = 3.0;

        let mut cmd = EditUserParamBindingCommand::new_with_reverse(
            effect_id,
            binding_id.clone(),
            BindingMappingEdit {
                scale: Some(3.0), // new = the dragged-to (final) value
                ..Default::default()
            },
            BindingMappingEdit {
                scale: Some(1.0), // explicit pre-drag reverse from the snapshot
                ..Default::default()
            },
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
