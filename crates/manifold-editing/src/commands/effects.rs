use crate::command::Command;
use crate::commands::effect_target::{EffectTarget, with_effects_mut};
use manifold_core::effects::{
    EffectInstance, ParamConvert, ParamEnvelope, ParamId, ParamSlot, ParameterDriver,
    UserParamBinding,
};
use manifold_core::project::Project;

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

/// Toggle an effect's enabled state.
#[derive(Debug)]
pub struct ToggleEffectCommand {
    target: EffectTarget,
    effect_index: usize,
    old_enabled: bool,
    new_enabled: bool,
}

impl ToggleEffectCommand {
    pub fn new(
        target: EffectTarget,
        effect_index: usize,
        old_enabled: bool,
        new_enabled: bool,
    ) -> Self {
        Self {
            target,
            effect_index,
            old_enabled,
            new_enabled,
        }
    }
}

impl Command for ToggleEffectCommand {
    fn execute(&mut self, project: &mut Project) {
        let idx = self.effect_index;
        let new_val = self.new_enabled;
        with_effects_mut(project, &self.target, |effects, _groups| {
            if let Some(effect) = effects.get_mut(idx) {
                effect.enabled = new_val;
            }
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let idx = self.effect_index;
        let old_val = self.old_enabled;
        with_effects_mut(project, &self.target, |effects, _groups| {
            if let Some(effect) = effects.get_mut(idx) {
                effect.enabled = old_val;
            }
        });
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
    target: EffectTarget,
    effect_index: usize,
    param_id: ParamId,
    old_value: f32,
    new_value: f32,
}

impl ChangeEffectParamCommand {
    pub fn new(
        target: EffectTarget,
        effect_index: usize,
        param_id: impl Into<ParamId>,
        old_value: f32,
        new_value: f32,
    ) -> Self {
        Self {
            target,
            effect_index,
            param_id: param_id.into(),
            old_value,
            new_value,
        }
    }
}

impl Command for ChangeEffectParamCommand {
    fn execute(&mut self, project: &mut Project) {
        let eidx = self.effect_index;
        let id = self.param_id.clone();
        let val = self.new_value;
        with_effects_mut(project, &self.target, |effects, _groups| {
            if let Some(effect) = effects.get_mut(eidx)
                && let Some(idx) = manifold_core::effect_definition_registry::param_id_to_index(
                    effect.effect_type(),
                    id.as_ref(),
                )
            {
                effect.set_base_param(idx, val);
            }
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let eidx = self.effect_index;
        let id = self.param_id.clone();
        let val = self.old_value;
        with_effects_mut(project, &self.target, |effects, _groups| {
            if let Some(effect) = effects.get_mut(eidx)
                && let Some(idx) = manifold_core::effect_definition_registry::param_id_to_index(
                    effect.effect_type(),
                    id.as_ref(),
                )
            {
                effect.set_base_param(idx, val);
            }
        });
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
    target: EffectTarget,
    effect_index: usize,
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
        target: EffectTarget,
        effect_index: usize,
        node_handle: String,
        inner_param: String,
        expose: bool,
        inner_meta: InnerParamMeta,
    ) -> Self {
        Self {
            target,
            effect_index,
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
        let eidx = self.effect_index;
        let node_handle = self.node_handle.clone();
        let inner_param = self.inner_param.clone();
        let expose = self.expose;
        let meta = self.inner_meta.clone();
        // `with_effects_mut` returns `Option<R>` from the closure; thread
        // the computed reverse-state out so we can stash it on the
        // command for undo. None when the target itself is missing.
        let reverse_out = with_effects_mut(project, &self.target, |effects, _groups| {
            let Some(effect) = effects.get_mut(eidx) else {
                return ReverseState::None;
            };
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
            // chain, not by index.
            let effect_type = with_effects_mut(project, &self.target, |effects, _| {
                effects.get(eidx).map(|fx| fx.effect_type().clone())
            })
            .flatten();
            if let (Some(effect_type), EffectTarget::Layer { layer_id }) = (effect_type, &self.target)
                && let Some((_, layer)) = project.timeline.find_layer_by_id_mut(layer_id)
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
        let eidx = self.effect_index;
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
        with_effects_mut(project, &self.target, |effects, _groups| {
            let Some(effect) = effects.get_mut(eidx) else {
                return;
            };
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
        });
        // Envelope restore — same Layer-vs-Master split as the prune
        // path. Master targets had nothing to capture, so this branch
        // no-ops uniformly there.
        if !envelopes_to_restore.is_empty()
            && let EffectTarget::Layer { layer_id } = &self.target
            && let Some((_, layer)) = project.timeline.find_layer_by_id_mut(layer_id)
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
    target: EffectTarget,
    effect_index: usize,
    param_index: usize,
    new_exposed: bool,
    /// Captured on first execute(). `None` when the call was a no-op.
    prev_exposed: Option<bool>,
}

impl ToggleStaticParamExposeCommand {
    pub fn new(
        target: EffectTarget,
        effect_index: usize,
        param_index: usize,
        new_exposed: bool,
    ) -> Self {
        Self {
            target,
            effect_index,
            param_index,
            new_exposed,
            prev_exposed: None,
        }
    }
}

impl Command for ToggleStaticParamExposeCommand {
    fn execute(&mut self, project: &mut Project) {
        let eidx = self.effect_index;
        let pidx = self.param_index;
        let new_v = self.new_exposed;
        let prev = with_effects_mut(project, &self.target, |effects, _groups| {
            let effect = effects.get_mut(eidx)?;
            let slot = effect.param_values.get_mut(pidx)?;
            if slot.exposed == new_v {
                return None;
            }
            let was = slot.exposed;
            slot.exposed = new_v;
            Some(was)
        });
        self.prev_exposed = prev.flatten();
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(prev) = self.prev_exposed else {
            return;
        };
        let eidx = self.effect_index;
        let pidx = self.param_index;
        with_effects_mut(project, &self.target, |effects, _groups| {
            if let Some(effect) = effects.get_mut(eidx)
                && let Some(slot) = effect.param_values.get_mut(pidx)
            {
                slot.exposed = prev;
            }
        });
    }

    fn description(&self) -> &str {
        if self.new_exposed {
            "Show Effect Param"
        } else {
            "Hide Effect Param"
        }
    }
}
