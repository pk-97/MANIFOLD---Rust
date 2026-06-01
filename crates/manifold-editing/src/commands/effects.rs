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
                && let Some(idx) = effect.param_id_to_value_index(id.as_ref())
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
                && let Some(idx) = effect.param_id_to_value_index(id.as_ref())
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
        }
    }
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
/// addressing (`target` + `effect_index` + `with_effects_mut`).
#[derive(Debug)]
pub struct EditUserParamBindingCommand {
    target: EffectTarget,
    effect_index: usize,
    /// Stable id of the binding being edited. NEVER mutated.
    binding_id: String,
    /// Post-edit values for the touched fields.
    new: BindingMappingEdit,
    /// Pre-edit values for the touched fields, captured on first
    /// execute(). `None` until then (and stays `None` if the target /
    /// binding doesn't resolve — a no-op).
    reverse: Option<BindingMappingEdit>,
}

impl EditUserParamBindingCommand {
    pub fn new(
        target: EffectTarget,
        effect_index: usize,
        binding_id: String,
        new: BindingMappingEdit,
    ) -> Self {
        Self {
            target,
            effect_index,
            binding_id,
            new,
            reverse: None,
        }
    }
}

impl Command for EditUserParamBindingCommand {
    fn execute(&mut self, project: &mut Project) {
        let eidx = self.effect_index;
        let binding_id = self.binding_id.clone();
        let new = self.new.clone();
        let reverse_out = with_effects_mut(project, &self.target, |effects, _groups| {
            let effect = effects.get_mut(eidx)?;
            let binding = effect
                .user_param_bindings
                .iter_mut()
                .find(|b| b.id == binding_id)?;
            // Snapshot the OLD values of exactly the touched fields
            // BEFORE applying, so undo restores precisely what changed.
            let reverse = new.snapshot_inverse(binding);
            new.apply_to(binding);
            // Bump so the renderer rebuilds the user-binding tail (and
            // drops its LastAppliedCache tail) on the next frame — the
            // new reshape applies immediately.
            effect.user_param_bindings_version =
                effect.user_param_bindings_version.wrapping_add(1);
            Some(reverse)
        });
        // `with_effects_mut` returns `Option<Option<_>>`; flatten the
        // missing-target / missing-binding cases into a single `None`.
        self.reverse = reverse_out.flatten();
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(reverse) = self.reverse.clone() else {
            return;
        };
        let eidx = self.effect_index;
        let binding_id = self.binding_id.clone();
        with_effects_mut(project, &self.target, |effects, _groups| {
            if let Some(effect) = effects.get_mut(eidx)
                && let Some(binding) = effect
                    .user_param_bindings
                    .iter_mut()
                    .find(|b| b.id == binding_id)
            {
                reverse.apply_to(binding);
                effect.user_param_bindings_version =
                    effect.user_param_bindings_version.wrapping_add(1);
            }
        });
    }

    fn description(&self) -> &str {
        "Edit param mapping"
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
    /// binding. Returns the project plus the binding id.
    fn project_with_one_user_binding() -> (Project, String) {
        let mut project = Project::default();
        let mut fx = EffectInstance::new(EffectTypeId::new("Mirror"));
        let binding_id = "user.uv_transform.rotation.1".to_string();
        fx.user_param_bindings.push(sample_user_binding(&binding_id));
        project.settings.master_effects.push(fx);
        (project, binding_id)
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
        let (mut project, binding_id) = project_with_one_user_binding();
        let v0 = project.settings.master_effects[0].user_param_bindings_version;

        let edit = BindingMappingEdit {
            label: Some("Spin".to_string()),
            min: Some(-2.0),
            max: Some(3.5),
            invert: Some(true),
            curve: Some(MacroCurve::SCurve),
        };
        let mut cmd = EditUserParamBindingCommand::new(
            EffectTarget::Master,
            0,
            binding_id.clone(),
            edit,
        );

        // ── execute: every touched field changes ──
        cmd.execute(&mut project);
        let b = master_binding(&project, &binding_id);
        assert_eq!(b.id, binding_id, "id must NEVER change");
        assert_eq!(b.label, "Spin");
        assert_eq!(b.min, -2.0);
        assert_eq!(b.max, 3.5);
        assert!(b.invert);
        assert_eq!(b.curve, MacroCurve::SCurve);
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
    }

    #[test]
    fn edit_mapping_only_touches_some_fields() {
        // A partial edit (only invert + curve) must leave label / min /
        // max untouched, and undo must restore exactly those two.
        let (mut project, binding_id) = project_with_one_user_binding();
        let edit = BindingMappingEdit {
            invert: Some(true),
            curve: Some(MacroCurve::Exponential),
            ..Default::default()
        };
        let mut cmd = EditUserParamBindingCommand::new(
            EffectTarget::Master,
            0,
            binding_id.clone(),
            edit,
        );
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
        let (mut project, _binding_id) = project_with_one_user_binding();
        let v0 = project.settings.master_effects[0].user_param_bindings_version;
        let mut cmd = EditUserParamBindingCommand::new(
            EffectTarget::Master,
            0,
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
}
