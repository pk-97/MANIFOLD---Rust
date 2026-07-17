use crate::command::Command;
use crate::commands::effect_target::{EffectTarget, with_effects_mut};
use manifold_core::{EffectId, GraphTarget};
use manifold_core::effects::{
    PresetInstance, ParamConvert, ParamEnvelope, ParamId, ParameterDriver,
    UserParamBinding,
};
use manifold_core::params::Param;
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

// ─── "3D Shading" relight (docs/DEPTH_RELIGHT_DESIGN.md, phase P5) ────
//
// Both commands address a [`GraphTarget`] (works uniformly for an effect
// card or a generator card — `PresetInstance` is the shared home for
// `relight`/`relight_params` on both kinds) and resolve it via
// `Project::preset_instance_mut`, the same const-twin accessor
// `ChangeGraphParamCommand` above reads through `with_preset_graph_mut`.
// Neither the toggle nor a knob edit touches `PresetInstance::graph` — the
// relight template is synthesized at splice time
// (`relight::relight_augment`), not authored into the def — so there is no
// topology to diff. `bump_graph_structure_version()` still runs on every
// edit: it's what the renderer's rebuild-detection compares against for the
// UI graph snapshot, and cheap insurance alongside the dedicated
// `PresetInstance.relight`/`relight_params` comparison the renderer's
// per-frame sweeps do (`preset_runtime::compute_topology_hash`,
// `generator_renderer`'s `applied_relight`).

/// Toggle the "3D Shading" card toggle. Undo-able like any other card
/// control.
#[derive(Debug)]
pub struct ToggleRelightCommand {
    target: GraphTarget,
    old_relight: bool,
    new_relight: bool,
}

impl ToggleRelightCommand {
    pub fn new(target: GraphTarget, old_relight: bool, new_relight: bool) -> Self {
        Self {
            target,
            old_relight,
            new_relight,
        }
    }
}

impl Command for ToggleRelightCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(inst) = project.preset_instance_mut(&self.target) {
            inst.relight = self.new_relight;
            inst.bump_graph_structure_version();
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(inst) = project.preset_instance_mut(&self.target) {
            inst.relight = self.old_relight;
            inst.bump_graph_structure_version();
        }
    }

    fn description(&self) -> &str {
        "Toggle 3D Shading"
    }
}

/// Addresses one of [`manifold_core::effects::RelightParams`]'s numeric
/// knobs (D3) — the card's Light X/Y, Relief, AO Intensity, Shadow
/// Softness, and Gain sliders each drive one variant. One command type
/// instead of six near-identical structs; `field` selects the target so
/// undo/redo replay against the right slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelightField {
    LightX,
    LightY,
    Relief,
    AoIntensity,
    ShadowSoftness,
    Gain,
}

impl RelightField {
    fn get(self, p: &manifold_core::effects::RelightParams) -> f32 {
        match self {
            Self::LightX => p.light_x,
            Self::LightY => p.light_y,
            Self::Relief => p.relief,
            Self::AoIntensity => p.ao_intensity,
            Self::ShadowSoftness => p.shadow_softness,
            Self::Gain => p.gain,
        }
    }

    fn set(self, p: &mut manifold_core::effects::RelightParams, value: f32) {
        match self {
            Self::LightX => p.light_x = value,
            Self::LightY => p.light_y = value,
            Self::Relief => p.relief = value,
            Self::AoIntensity => p.ao_intensity = value,
            Self::ShadowSoftness => p.shadow_softness = value,
            Self::Gain => p.gain = value,
        }
    }
}

/// Change one relight knob (D3). Always live on the instance regardless of
/// whether the toggle is on — see `RelightParams`'s doc: the card renders
/// these rows greyed, never hidden, when the toggle is off, so editing while
/// off must still take effect for when it's switched on.
#[derive(Debug)]
pub struct SetRelightParamCommand {
    target: GraphTarget,
    field: RelightField,
    old_value: f32,
    new_value: f32,
}

impl SetRelightParamCommand {
    pub fn new(target: GraphTarget, field: RelightField, old_value: f32, new_value: f32) -> Self {
        Self {
            target,
            field,
            old_value,
            new_value,
        }
    }

    /// Build from the live instance's current value (so callers don't have
    /// to read + clone `RelightParams` themselves before constructing).
    pub fn from_current(
        project: &Project,
        target: GraphTarget,
        field: RelightField,
        new_value: f32,
    ) -> Option<Self> {
        let old_value = field.get(&project.preset_instance(&target)?.relight_params);
        Some(Self::new(target, field, old_value, new_value))
    }
}

impl Command for SetRelightParamCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(inst) = project.preset_instance_mut(&self.target) {
            self.field.set(&mut inst.relight_params, self.new_value);
            inst.bump_graph_structure_version();
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(inst) = project.preset_instance_mut(&self.target) {
            self.field.set(&mut inst.relight_params, self.old_value);
            inst.bump_graph_structure_version();
        }
    }

    fn description(&self) -> &str {
        "Change 3D Shading Param"
    }
}

/// Change the D4 "Height From" enum (Auto / Luminance / Inverted Luminance).
#[derive(Debug)]
pub struct SetRelightHeightFromCommand {
    target: GraphTarget,
    old_value: manifold_core::effects::RelightHeightFrom,
    new_value: manifold_core::effects::RelightHeightFrom,
}

impl SetRelightHeightFromCommand {
    pub fn new(
        target: GraphTarget,
        old_value: manifold_core::effects::RelightHeightFrom,
        new_value: manifold_core::effects::RelightHeightFrom,
    ) -> Self {
        Self {
            target,
            old_value,
            new_value,
        }
    }
}

impl Command for SetRelightHeightFromCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(inst) = project.preset_instance_mut(&self.target) {
            inst.relight_params.height_from = self.new_value;
            inst.bump_graph_structure_version();
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(inst) = project.preset_instance_mut(&self.target) {
            inst.relight_params.height_from = self.old_value;
            inst.bump_graph_structure_version();
        }
    }

    fn description(&self) -> &str {
        "Change 3D Shading Height Source"
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
        /// The removed manifest entry, carrying value + base + exposure +
        /// calibration so undo reinstates the exact snapshot with a single
        /// `restore_user_binding_at`.
        param: Param,
        position: usize,
        /// Drivers pruned from `PresetInstance.drivers` because their
        /// `param_id` matched the removed binding's id.
        removed_drivers: Vec<ParameterDriver>,
        /// Ableton mappings pruned from
        /// `PresetInstance.ableton_mappings` for the same reason.
        removed_ableton_mappings: Vec<manifold_core::ableton_mapping::AbletonParamMapping>,
        /// Envelopes pruned from `PresetInstance.envelopes` whose
        /// `param_id` matched the removed binding's id — envelope-home
        /// unification put envelopes on the instance alongside drivers and
        /// Ableton mappings, so they prune and restore in the same borrow.
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
                    value_labels: Vec::new(),
                    section: None,
                };
                effect.append_user_binding(binding);
                ReverseState::Exposed { user_param_id: id }
            } else {
                // Unexpose: remove the binding matching this addressing.
                let Some(position) = existing_position else {
                    return ReverseState::None;
                };
                let user_param_id = user_bindings[position].id.clone();
                // Capture the full manifest entry BEFORE removal so undo
                // reinstates the exact pre-modulation snapshot (value + base +
                // calibration) in one insert. The entry is guaranteed present
                // for a live user binding: `append_user_binding` /
                // `remove_user_binding_by_id` keep the binding and its manifest
                // entry coupled by id, so there is no positional mismatch to
                // defend against as there was under the old value-index model.
                let param = effect
                    .params
                    .get(&user_param_id)
                    .cloned()
                    .expect("manifest entry present for a live user binding");
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
                // Envelope-home unification: envelopes ride on the instance
                // now, so they prune in the same borrow as drivers / Ableton
                // mappings (no separate layer pass).
                let removed_envelopes = if let Some(es) = effect.envelopes.as_mut() {
                    let mut taken = Vec::new();
                    es.retain(|e| {
                        let keep = e.param_id != user_param_id;
                        if !keep {
                            taken.push(e.clone());
                        }
                        keep
                    });
                    if es.is_empty() {
                        effect.envelopes = None;
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
                    param,
                    position,
                    removed_drivers,
                    removed_ableton_mappings,
                    removed_envelopes,
                }
            }
        });
        self.reverse = reverse_out.unwrap_or_default();
    }

    fn undo(&mut self, project: &mut Project) {
        let reverse = std::mem::take(&mut self.reverse);
        if let Some(effect) = project.find_effect_by_id_mut(&self.effect_id) {
            match reverse {
                ReverseState::None => {}
                ReverseState::Exposed { user_param_id } => {
                    effect.remove_user_binding_by_id(&user_param_id);
                }
                ReverseState::Unexposed {
                    binding,
                    param,
                    position,
                    removed_drivers,
                    removed_ableton_mappings,
                    removed_envelopes,
                } => {
                    // Re-insert at original position so the user-tail
                    // slot positions stay stable for any other addressing
                    // (drivers, Ableton mappings) that referenced the
                    // user_param_id by string.
                    // The restored manifest entry carries its own `base`, so
                    // restore_user_binding_at reinstates the exact pre-modulation
                    // snapshot — no separate base override needed.
                    effect.restore_user_binding_at(binding, position, param);
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
                    // Envelopes restore onto the instance in the same borrow.
                    if !removed_envelopes.is_empty() {
                        effect
                            .envelopes
                            .get_or_insert_with(Vec::new)
                            .extend(removed_envelopes);
                    }
                }
            }
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

/// Toggle the `exposed` flag on a param by its id. Hidden params
/// disappear from the effect card slider list but keep their value,
/// driver, and Ableton mapping intact — they're still addressable by
/// `param_id` for OSC / driver / mapping write paths.
///
/// Symmetric undo: stores the previous flag value (which is just `!new`
/// for non-no-op execution). No-op when the param already matches the
/// requested state, or when `param_id` is not in the manifest.
#[derive(Debug)]
pub struct ToggleParamExposeCommand {
    effect_id: EffectId,
    param_id: ParamId,
    new_exposed: bool,
    /// Captured on first execute(). `None` when the call was a no-op.
    prev_exposed: Option<bool>,
}

impl ToggleParamExposeCommand {
    pub fn new(effect_id: EffectId, param_id: ParamId, new_exposed: bool) -> Self {
        Self {
            effect_id,
            param_id,
            new_exposed,
            prev_exposed: None,
        }
    }
}

impl Command for ToggleParamExposeCommand {
    fn execute(&mut self, project: &mut Project) {
        let id = self.param_id.as_ref();
        let new_v = self.new_exposed;
        self.prev_exposed = project.find_effect_by_id_mut(&self.effect_id).and_then(|effect| {
            if !effect.params.contains(id) {
                return None;
            }
            let was = effect.is_param_exposed(id);
            if was == new_v {
                return None;
            }
            effect.set_param_exposed(id, new_v);
            Some(was)
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(prev) = self.prev_exposed else {
            return;
        };
        if let Some(effect) = project.find_effect_by_id_mut(&self.effect_id) {
            effect.set_param_exposed(self.param_id.as_ref(), prev);
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
    /// Card-bundling section name (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §2
    /// D5). Outer `Option` = "this edit touches the field" (the usual
    /// `BindingMappingEdit` convention); inner `Option<String>` = the new
    /// value, where `None` clears the row back to unsectioned. Manifest-only
    /// (BOUNDARIES D4) — applied by [`Self::apply_to_manifest_spec`] like
    /// every other spec field here; never written to `meta.params`.
    pub section: Option<Option<String>>,
}

impl BindingMappingEdit {
    /// Apply the label/min/max/invert/curve fields onto the LIVE manifest
    /// entry's spec — the sole authority a card/renderer reads
    /// (PARAM_STORAGE_DESIGN.md D6). Returns the PRE-edit values for undo.
    ///
    /// PARAM_STORAGE_BOUNDARIES_DESIGN.md D4: the graph's `preset_metadata
    /// .params` (`meta.params`) is a save-time-derived shadow now, not a
    /// second live target — this method never touches it. Writing these
    /// fields onto both the manifest AND `meta.params` was exactly the
    /// dual-write P2 deletes (two owners of one fact).
    fn apply_to_manifest_spec(
        &self,
        spec: &mut manifold_core::effect_graph_def::ParamSpecDef,
    ) -> SpecReshapeSnapshot {
        let prev = SpecReshapeSnapshot {
            name: spec.name.clone(),
            min: spec.min,
            max: spec.max,
            invert: spec.invert,
            curve: spec.curve,
            section: spec.section.clone(),
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
        if let Some(section) = &self.section {
            spec.section = section.clone();
        }
        prev
    }

    /// Apply the scale/offset fields onto the graph's live `BindingDef` — the
    /// ONLY home for them (no manifest field: scale/offset live on the
    /// binding recipe, which synth reads directly, per PARAM_STORAGE_DESIGN
    /// D6). Unlike the spec fields above, this write is NOT part of the
    /// `meta.params` dual-write P2 deletes — it survives untouched. Returns
    /// the PRE-edit `(scale, offset)` for undo.
    fn apply_scale_offset(
        &self,
        binding: &mut manifold_core::effect_graph_def::BindingDef,
    ) -> (f32, f32) {
        let prev = (binding.scale, binding.offset);
        if let Some(scale) = self.scale {
            binding.scale = scale;
        }
        if let Some(offset) = self.offset {
            binding.offset = offset;
        }
        prev
    }
}

/// Pre-edit snapshot of the manifest spec half of a reshape edit, for undo.
#[derive(Debug, Clone)]
struct SpecReshapeSnapshot {
    name: String,
    min: f32,
    max: f32,
    invert: bool,
    curve: manifold_core::macro_bank::MacroCurve,
    section: Option<String>,
}

impl SpecReshapeSnapshot {
    fn restore(&self, spec: &mut manifold_core::effect_graph_def::ParamSpecDef) {
        spec.name = self.name.clone();
        spec.min = self.min;
        spec.max = self.max;
        spec.invert = self.invert;
        spec.curve = self.curve;
        spec.section = self.section.clone();
    }
}

/// Full pre-edit reverse for the single-shot undo path: the manifest spec
/// snapshot (always captured — the manifest entry is guaranteed by the
/// `execute()` guard) plus the graph binding's pre-edit `(scale, offset)`,
/// `None` when the graph carried no matching `BindingDef` (nothing to
/// restore there; the manifest half still applies unconditionally).
#[derive(Debug, Clone)]
struct MappingReverse {
    spec: SpecReshapeSnapshot,
    scale: Option<f32>,
    offset: Option<f32>,
}

/// Edit one card param's reshape — its display label, min/max range, invert
/// flag, response curve, or scale/offset — on an effect or generator addressed
/// by a [`GraphTarget`].
///
/// The reshape lives in TWO places now (PARAM_STORAGE_BOUNDARIES_DESIGN.md D4):
/// label/min/max/invert/curve on the instance's live [`manifold_core::params
/// ::ParamManifest`] entry (the sole authority a card/renderer reads); scale/
/// offset on the instance's per-instance graph override's `BindingDef` (their
/// only home). The per-instance graph override (`graph` for an effect,
/// `generator_graph` for a generator) is materialized from the caller-supplied
/// `seed_def` first if the instance is still on the catalog default (`graph:
/// None`), so a recalibration becomes a per-instance override exactly like a
/// topology edit — the manifest already exists regardless, seeded at
/// instantiation/load. The param is addressed by its stable id (never mutated
/// — drivers/Ableton/OSC reference it).
///
/// On first execute the pre-edit values are snapshotted for undo.
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
    reverse: Option<MappingReverse>,
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

/// Apply the scale/offset half of `edit` onto `binding_id`'s live
/// `BindingDef`, if the graph override carries a matching one. Returns the
/// PRE-edit `(scale, offset)`, or `None` if there was no matching binding to
/// touch (nothing to restore there on undo). Shared by `execute` and both
/// undo paths so the "find the binding, apply, bump the graph version"
/// sequence lives in one place.
fn apply_scale_offset_on_graph(
    host: &mut PresetInstance,
    binding_id: &str,
    edit: &BindingMappingEdit,
) -> Option<(f32, f32)> {
    let prev = host
        .graph_def_mut()
        .as_mut()
        .and_then(|g| g.preset_metadata.as_mut())
        .and_then(|meta| meta.bindings.iter_mut().find(|b| b.id == binding_id))
        .map(|b| edit.apply_scale_offset(b));
    host.bump_graph_version();
    prev
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
                // still on the catalog default, so scale/offset (the binding
                // half of the reshape) have a home.
                if host.graph_def().is_none() {
                    let seed = seed_def?;
                    *host.graph_def_mut() = Some(seed);
                    host.bump_graph_structure_version();
                }
                // Guard: nothing to calibrate if the manifest doesn't carry
                // this id (mirrors the old `meta.params.find(...)?` guard,
                // now against the manifest — the actual authority a
                // card/renderer reads. PARAM_STORAGE_BOUNDARIES_DESIGN.md D4).
                host.params.get(&binding_id)?;

                // scale/offset have no manifest home (they live on the
                // BindingDef, which synth reads directly) — apply them onto
                // the graph's binding, the only place they live. NOT part of
                // the `meta.params` dual-write P2 deletes.
                let prev_binding = apply_scale_offset_on_graph(host, &binding_id, &new);

                // The manifest entry's `spec` is the LIVE reshape the
                // renderer reads (`synth_user_binding` for a user param,
                // calibration for a stock one — PARAM_STORAGE_DESIGN.md D6).
                // `meta.params` on the graph is derived from the manifest at
                // serialize time now (D12) — writing it here too would be
                // the exact dual-write P2 deletes, so this is the SOLE spec
                // write.
                let p = host.params.get_mut(&binding_id)?;
                let spec_snap = new.apply_to_manifest_spec(&mut p.spec);
                p.calibrated = true;

                Some(MappingReverse {
                    spec: spec_snap,
                    scale: prev_binding.map(|(s, _)| s),
                    offset: prev_binding.map(|(_, o)| o),
                })
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
                apply_scale_offset_on_graph(host, &binding_id, &reverse_edit);
                if let Some(p) = host.params.get_mut(&binding_id) {
                    reverse_edit.apply_to_manifest_spec(&mut p.spec);
                }
            });
            return;
        }
        // Single-shot: restore the self-captured pre-edit snapshot.
        let Some(reverse) = self.reverse.clone() else {
            return;
        };
        project.with_preset_graph_mut(&self.target, |host| {
            if let (Some(scale), Some(offset)) = (reverse.scale, reverse.offset)
                && let Some(b) = host
                    .graph_def_mut()
                    .as_mut()
                    .and_then(|g| g.preset_metadata.as_mut())
                    .and_then(|meta| meta.bindings.iter_mut().find(|b| b.id == binding_id))
            {
                b.scale = scale;
                b.offset = offset;
            }
            host.bump_graph_version();
            // Restore the manifest spec (see execute) — the sole live authority.
            if let Some(p) = host.params.get_mut(&binding_id) {
                reverse.spec.restore(&mut p.spec);
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
            value_labels: Vec::new(),
            section: None,
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
                    is_angle: false,
                    is_trigger_gate: false,
                    wraps: false,
                    section: None,
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
    /// instance's per-instance graph spec + binding. Post-P2
    /// (PARAM_STORAGE_BOUNDARIES_DESIGN.md D4) `invert`/`max` here are only
    /// the SEEDED values (the graph's `preset_metadata.params` is a
    /// save-time-derived shadow now, not a live target) — `scale` is still
    /// live, since it has no manifest home. Use [`manifest_reshape`] to read
    /// the live invert/max after a calibration edit.
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

    /// Read a param's live `(invert, max)` off the MANIFEST — the sole
    /// authority a card/renderer reads after a calibration edit
    /// (PARAM_STORAGE_BOUNDARIES_DESIGN.md D4).
    fn manifest_reshape(inst: &manifold_core::effects::PresetInstance, id: &str) -> (bool, f32) {
        let p = inst.params.get(id).expect("manifest entry present");
        (p.spec.invert, p.spec.max)
    }

    /// Manually seed `inst`'s manifest with a bundled [`Param`] matching
    /// [`seed_def_with_param`]'s `ParamSpecDef` — mirrors what
    /// `build_param_manifest` does for a REAL registered preset at
    /// instantiation. `manifold-editing` doesn't depend on
    /// `manifold-renderer` (no registry to consult in this crate's tests),
    /// so `EditParamMappingCommand`'s manifest-first guard needs the entry
    /// seeded by hand here.
    fn seed_manifest_param(inst: &mut manifold_core::effects::PresetInstance, param_id: &str) {
        use manifold_core::effect_graph_def::ParamSpecDef;
        inst.params.push(Param::bundled(ParamSpecDef {
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
            is_angle: false,
            is_trigger_gate: false,
            wraps: false,
            section: None,
        }));
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
            section: None,
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
    fn edit_mapping_writes_and_undoes_section() {
        // D5 (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §2): the calibration
        // popover's section edit — manifest-only (BOUNDARIES D4), same
        // one-shot execute/undo shape as every other reshape field here.
        let (mut project, effect_id, binding_id) = project_with_one_user_binding();
        let edit = BindingMappingEdit {
            section: Some(Some("Leaf".to_string())),
            ..Default::default()
        };
        let mut cmd = EditParamMappingCommand::new(
            GraphTarget::Effect(effect_id.clone()),
            binding_id.clone(),
            edit,
            None,
        );
        cmd.execute(&mut project);
        let b = master_binding(&project, &binding_id);
        assert_eq!(b.section.as_deref(), Some("Leaf"));
        // Untouched fields keep their original values.
        assert_eq!(b.label, "Original Label");

        cmd.undo(&mut project);
        let b = master_binding(&project, &binding_id);
        assert_eq!(b.section, None, "undo restores the pre-edit (absent) section");

        // Clearing back to unsectioned: `Some(None)` is a real edit, distinct
        // from `None` (untouched). Re-set it, then clear it.
        cmd.execute(&mut project);
        assert_eq!(master_binding(&project, &binding_id).section.as_deref(), Some("Leaf"));
        let clear = BindingMappingEdit {
            section: Some(None),
            ..Default::default()
        };
        let mut clear_cmd =
            EditParamMappingCommand::new(GraphTarget::Effect(effect_id), binding_id.clone(), clear, None);
        clear_cmd.execute(&mut project);
        let b = master_binding(&project, &binding_id);
        assert_eq!(b.section, None, "Some(None) clears the section back to unsectioned");
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
    /// supplied `seed_def` (its scale/offset home) and calibrates the
    /// manifest spec (its invert/max home) — the post-P2 model
    /// (PARAM_STORAGE_BOUNDARIES_DESIGN.md D4). Undo restores both to their
    /// seeded values. `manifold-editing` has no renderer registry to seed
    /// the manifest from at instantiation, so the test hand-seeds it
    /// (`seed_manifest_param`) to mirror what a real registered preset gets
    /// for free via `build_param_manifest`.
    #[test]
    fn edit_stock_param_seeds_graph_and_roundtrips() {
        let mut project = Project::default();
        let fx = PresetInstance::new(PresetTypeId::new("ColorGrade"));
        let effect_id = fx.id.clone();
        project.settings.master_effects.push(fx);
        seed_manifest_param(&mut project.settings.master_effects[0], "amount");
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

        // execute: materializes the graph (for scale/offset's sake) + applies
        // invert/max to the manifest, scale to the graph binding.
        cmd.execute(&mut project);
        assert!(
            project.settings.master_effects[0].graph.is_some(),
            "editing a stock param materializes the per-instance graph",
        );
        assert_eq!(
            manifest_reshape(&project.settings.master_effects[0], "amount"),
            (true, 5.0),
            "invert/max land on the manifest — the sole live authority",
        );
        let (_, _, scale) = spec_reshape(&project.settings.master_effects[0].graph, "amount");
        assert_eq!(scale, 2.0, "scale lands on the materialized graph's binding");
        assert_ne!(
            project.settings.master_effects[0].graph_version, v0,
            "editing a stock param bumps graph_version",
        );

        // undo: the seeded values are restored on both sides.
        cmd.undo(&mut project);
        assert_eq!(
            manifest_reshape(&project.settings.master_effects[0], "amount"),
            (false, 1.0),
            "undo restores the seeded manifest spec (invert false, max 1.0)",
        );
        let (_, _, scale) = spec_reshape(&project.settings.master_effects[0].graph, "amount");
        assert_eq!(scale, 1.0, "undo restores scale to identity on the graph binding");
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
        // Unlike an effect type (ColorGrade), "Plasma" is a compiled-in
        // generator registration (`generator_metadata_submissions.rs`) that
        // `manifold-core` itself carries — reachable without the renderer's
        // registry — so `Layer::new_generator`'s `init_defaults()` already
        // seeds the manifest with a real "complexity" entry (min 0.0, max
        // 1.0, invert false); no manual seed needed here.
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
            // invert: the manifest, the sole live authority (D4). scale: the
            // graph binding, its only home.
            let (invert, _max) = manifest_reshape(gp, "complexity");
            let (_, _, scale) = spec_reshape(&gp.graph, "complexity");
            (invert, scale)
        };

        // execute: materializes generator_graph (scale's home) + calibrates
        // the manifest (invert's home).
        cmd.execute(&mut project);
        assert_eq!(reshape(&project, layer_id.as_str()), (true, 2.0));

        // undo restores the seeded values.
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
