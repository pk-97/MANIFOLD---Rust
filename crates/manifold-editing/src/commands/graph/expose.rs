//! Node-param exposure + effect-side mirroring — `ToggleNodeParamExposeCommand`
//! and its reverse-state machinery. Split out of `graph.rs` in P2-G/S3 (pure
//! move). Target-graph helpers, `descend_level`, and `innermost_group_display_name`
//! stay in `graph/mod.rs` (and, from S4, `graph/groups.rs`) and are used via `super`.

use manifold_core::GraphTarget;
use manifold_core::NodeId;
use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, SerializedParamValue};
use manifold_core::project::Project;

use crate::command::Command;

use super::{
    descend_level, innermost_group_display_name, with_existing_target_graph_mut,
    with_target_graph_mut,
};

// ---------------------------------------------------------------------------
// Toggle Node Param Expose (unified Effect + Generator)
// ---------------------------------------------------------------------------

/// Toggle whether an inner-graph parameter is exposed on the outer
/// card. **Single command for both Effect-hosted and Generator-hosted
/// graphs** — the graph editor is one surface, the click handler emits
/// one [`crate::PanelAction`], and exposure state lives in one place
/// (the graph node's `exposed_params` set).
///
/// For Effect targets, this command also mirrors the new state into
/// the legacy `PresetInstance.param_values[i].exposed` (for params
/// covered by a preset binding's static-block slot) and
/// [`PresetInstance::user_param_bindings`] (for inner-node params with
/// no preset binding). The mirror is what keeps the timeline-card
/// state-sync path working until Step 8 of the unification cuts those
/// fields over to the graph as the single source of truth.
///
/// For Generator targets, only the graph write happens — generators
/// never had a legacy `param_values` shadow.
#[derive(Debug)]
pub struct ToggleNodeParamExposeCommand {
    target: GraphTarget,
    /// Stable [`NodeId`] of the inner node — the identity the *mirror* side
    /// stores (the preset `BindingTarget::Node`, the `UserParamBinding.node_id`).
    /// NOT used to locate the node in the graph: it's empty on bundled-preset
    /// nodes, so the graph-side `exposed_params` write addresses by
    /// `(scope_path, node_u32_id)` instead — see [`Self::node_u32_id`].
    node_id: NodeId,
    /// Runtime (doc) id of the inner node, addressed at [`Self::scope_path`] —
    /// the same `(scope, id)` key every other graph command uses to reach a
    /// node (nested groups included). Always populated, so it locates the node
    /// where the stable `node_id` can't.
    node_u32_id: u32,
    /// View depth this edit targets — a path of group ids (empty = document
    /// root). Lets exposure reach a param on a node the user has descended
    /// into. See [`descend_level`].
    scope_path: Vec<u32>,
    /// Current display handle, used only to mint readable
    /// `user.<handle>.<param>.<n>` ids. Not an addressing role.
    node_handle: String,
    inner_param: String,
    expose: bool,
    catalog_default: EffectGraphDef,
    /// Inner-node ParamDef metadata captured at panel-build time.
    /// Required when the Effect-side mirror needs to append a new
    /// `UserParamBinding` — the binding needs label/min/max/default/
    /// convert to be well-formed. Generators ignore this.
    inner_meta: Option<manifold_core::effects::ParamConvert>,
    /// Angle presentation hint for the inner param, captured at panel-build
    /// time from `ParamType::Angle`. Flows onto the appended
    /// `UserParamBinding` so the card slider shows degrees. Display-only —
    /// storage stays radians.
    inner_is_angle: bool,
    /// Enum option labels for the inner param, captured at panel-build time
    /// from the live `ParamDef`. Flows onto the appended `UserParamBinding`
    /// (and its `ParamSpecDef`) so an exposed enum renders as a labelled
    /// stepped card slider instead of a bare numeric one. Empty for non-enums.
    inner_value_labels: Vec<String>,
    /// Display label for the user binding (effect-side only).
    inner_label: String,
    inner_min: f32,
    inner_max: f32,
    inner_default: f32,
    /// Reverse state, populated on first execute(). See
    /// [`NodeExposeReverse`].
    reverse: NodeExposeReverse,
}

// Two-variant undo-state enum: `None` until execute() runs, then
// `Captured` carries everything needed to reverse the toggle. The
// `Captured` variant grew past the clippy size threshold when the
// envelope-cleanup work landed, but boxing the captured payload
// would add heap traffic to every undoable graph-toggle command
// for no real win — these structs live in an undo stack capped at
// 200 entries, not on any hot path. Lint suppressed deliberately.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Default)]
enum NodeExposeReverse {
    #[default]
    None,
    /// Captured on execute. Restored on undo.
    Captured {
        /// Previous membership of `inner_param` in the node's
        /// `exposed_params` set. `true` if it was present before
        /// execute, `false` otherwise. Restored unconditionally on undo.
        prev_in_set: bool,
        /// Mirror reverse state. Mirror collapse: effect and generator
        /// targets both run through `mirror_effect_side` over the target's
        /// `&mut PresetInstance` (the generator graph lives on `gen_params`),
        /// so there is one reverse type for both.
        mirror: EffectMirrorReverse,
    },
}

// `RemovedUserBinding` is large because it captures the full
// `UserParamBinding` + every orphaned driver / Ableton mapping /
// envelope so undo can faithfully restore the pre-unexpose state.
// Boxing it would only shrink the enum footprint on the undo
// stack — the captured payload lives there for at most ~200
// commands and is never on a render hot path, so the indirection
// trade isn't worth it.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
enum EffectMirrorReverse {
    /// The (handle, param) maps to a bundled-prefix param; we flipped its
    /// exposure via `set_param_exposed`. Undo restores `prev_exposed`.
    StaticSlot { param_id: String, prev_exposed: bool },
    /// The (handle, param) is a non-preset param; we appended a
    /// `UserParamBinding`. Undo removes it by id.
    AppendedUserBinding {
        user_param_id: String,
    },
    /// The (handle, param) is a non-preset param; we removed an
    /// existing `UserParamBinding`. Undo reinserts it at `position`
    /// with the captured manifest entry, plus re-attaches any orphaned
    /// drivers / Ableton mappings / envelopes that referenced the
    /// binding's id.
    RemovedUserBinding {
        binding: manifold_core::effects::UserParamBinding,
        position: usize,
        param: manifold_core::params::Param,
        /// Drivers pruned from `PresetInstance.drivers` because their
        /// `param_id` matched the removed binding's id. Without this
        /// pruning the rows would survive in the project file but
        /// never resolve to a target, leaving silently-dead
        /// automation behind.
        removed_drivers: Vec<manifold_core::effects::ParameterDriver>,
        /// Ableton mappings pruned for the same reason.
        removed_ableton_mappings:
            Vec<manifold_core::ableton_mapping::AbletonParamMapping>,
        /// Envelopes pruned from `PresetInstance.envelopes` whose
        /// `param_id` matched the removed binding's id. Envelope-home
        /// unification put envelopes on the instance, so they prune and
        /// restore in the same effect borrow as drivers / Ableton
        /// mappings (no separate layer pass).
        removed_envelopes: Vec<manifold_core::effects::ParamEnvelope>,
    },
    /// No-op: the Effect-side state already matched the requested
    /// state (idempotent re-toggle). Nothing to undo on the mirror.
    NoOp,
}

impl ToggleNodeParamExposeCommand {
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        target: GraphTarget,
        node_id: NodeId,
        node_u32_id: u32,
        node_handle: String,
        inner_param: String,
        expose: bool,
        catalog_default: EffectGraphDef,
        inner_label: String,
        inner_min: f32,
        inner_max: f32,
        inner_default: f32,
        inner_convert: manifold_core::effects::ParamConvert,
        inner_is_angle: bool,
        inner_value_labels: Vec<String>,
    ) -> Self {
        Self {
            target,
            node_id,
            node_u32_id,
            scope_path: Vec::new(),
            node_handle,
            inner_param,
            expose,
            catalog_default,
            inner_meta: Some(inner_convert),
            inner_is_angle,
            inner_value_labels,
            inner_label,
            inner_min,
            inner_max,
            inner_default,
            reverse: NodeExposeReverse::None,
        }
    }

    /// Target a nested group level instead of the document root. Matches the
    /// `with_scope` builder every other graph command exposes.
    pub fn with_scope(mut self, scope_path: Vec<u32>) -> Self {
        self.scope_path = scope_path;
        self
    }
}

/// Flip `inner_param` membership in the `exposed_params` set of the node with
/// doc id `node_u32_id` within `nodes` (a single, already-descended graph
/// level). Returns the previous membership for undo, or `None` if the level has
/// no node with that id. Matches by the always-populated u32 doc id — the same
/// key `SetGraphNodeParamCommand` uses — because a bundled node's stable
/// `node_id` is empty and can't locate anything.
fn flip_node_exposed(
    nodes: &mut [EffectGraphNode],
    node_u32_id: u32,
    inner_param: &str,
    expose: bool,
) -> Option<bool> {
    let node = nodes.iter_mut().find(|n| n.id == node_u32_id)?;
    let was = node.exposed_params.contains(inner_param);
    if expose {
        node.exposed_params.insert(inner_param.to_string());
    } else {
        node.exposed_params.remove(inner_param);
    }
    Some(was)
}

/// Walk every binding in `def.preset_metadata.bindings` and ensure
/// the matching node's `exposed_params` set contains the target param.
/// Called by the expose command to materialise the implicit
/// preset-driven defaults before applying a user toggle. After the
/// first materialisation, `into_graph`'s binding backfill becomes a
/// no-op (it short-circuits when the def already carries explicit
/// exposure entries), so unchecks stick across save/reload.
fn materialize_binding_exposures(def: &mut EffectGraphDef) {
    use manifold_core::effect_graph_def::BindingTarget;
    let Some(meta) = def.preset_metadata.as_ref() else {
        return;
    };
    // Collect the (node_id, param) pairs first; we can't borrow meta
    // immutably while mutating nodes.
    let pairs: Vec<(NodeId, String)> = meta
        .bindings
        .iter()
        .filter_map(|b| match &b.target {
            BindingTarget::Node { node_id, param } => {
                Some((node_id.clone(), param.clone()))
            }
            BindingTarget::Composite { .. } => None,
        })
        .collect();
    for (node_id, param) in pairs {
        if let Some(node) = def.nodes.iter_mut().find(|n| n.node_id == node_id) {
            node.exposed_params.insert(param);
        }
    }
}

/// Restore `inner_param` membership in the `exposed_params` set of the node with
/// doc id `node_u32_id` within `nodes` (an already-descended level) to
/// `prev_in_set`. Idempotent — silently no-ops if the node is gone.
fn restore_node_exposed(
    nodes: &mut [EffectGraphNode],
    node_u32_id: u32,
    inner_param: &str,
    prev_in_set: bool,
) {
    if let Some(node) = nodes.iter_mut().find(|n| n.id == node_u32_id) {
        if prev_in_set {
            node.exposed_params.insert(inner_param.to_string());
        } else {
            node.exposed_params.remove(inner_param);
        }
    }
}

/// Find the static-block param slot index for a `(node_id, inner_param)`
/// pair, by scanning the preset metadata's bindings. Returns the
/// position in `metadata.params` of the binding whose target is
/// `(node_id, param)`. `None` if the def has no metadata or no binding
/// targets that `(node_id, param)`.
fn static_slot_for(
    def: &EffectGraphDef,
    node_id: &NodeId,
    inner_param: &str,
) -> Option<usize> {
    use manifold_core::effect_graph_def::BindingTarget;
    let meta = def.preset_metadata.as_ref()?;
    let binding_idx = meta.bindings.iter().position(|b| {
        // A user-added binding is NOT a static slot — it lives in the
        // user tail and is removed (not exposure-flipped) on unexpose.
        // Only bundled (shipped) bindings own a static `param_values` slot.
        if b.user_added {
            return false;
        }
        match &b.target {
            BindingTarget::Node { node_id: nid, param } => {
                nid == node_id && param == inner_param
            }
            BindingTarget::Composite { .. } => false,
        }
    })?;
    // Static-block slots are positional against `metadata.params` —
    // each `params[i]` corresponds to bindings sharing the same `id`.
    let binding_id = &meta.bindings[binding_idx].id;
    meta.params.iter().position(|p| &p.id == binding_id)
}

impl Command for ToggleNodeParamExposeCommand {
    fn execute(&mut self, project: &mut Project) {
        let node_handle = self.node_handle.clone();
        // Mirror-side identity for the card binding: apply the same "node_id
        // defaults to handle" convention the runtime graph loader uses
        // (`graph_loader.rs`), so a binding minted here targets the SAME
        // identity the chain resolves the node to. Bundled-preset nodes ship
        // with an empty stable `node_id`; without this the card slider would
        // bind to nothing and never drive the inner param. (The graph-side
        // `exposed_params` flip is located by `node_u32_id` below and doesn't
        // rely on this.)
        let node_id = if self.node_id.is_empty() {
            NodeId::new(node_handle.as_str())
        } else {
            self.node_id.clone()
        };
        let inner_param = self.inner_param.clone();
        let expose = self.expose;
        let inner_label = self.inner_label.clone();
        let inner_min = self.inner_min;
        let inner_max = self.inner_max;
        let inner_default = self.inner_default;
        let inner_convert = self.inner_meta.unwrap_or(manifold_core::effects::ParamConvert::Float);
        let inner_is_angle = self.inner_is_angle;
        let inner_value_labels = self.inner_value_labels.clone();

        // Graph-side write — flip the node's `exposed_params` membership and
        // locate the static-block slot (if any). Identical for both kinds:
        // `with_target_graph_mut` resolves the target's graph (an effect's, or
        // a layer generator's `gen_params.graph`). The node is located by
        // `(scope_path, node_u32_id)` — `descend_level` walks into the group the
        // user is viewing, then matches the always-populated doc id — because a
        // bundled node's stable `node_id` is empty and won't locate anything.
        let scope = self.scope_path.clone();
        let node_u32_id = self.node_u32_id;
        let graph_result: Option<(bool, Option<usize>, Option<String>)> = with_target_graph_mut(
            project,
            &self.target,
            &self.catalog_default,
            true,
            |def| {
                // Materialise bundled binding exposures + resolve the static slot
                // at the def level (both read `preset_metadata`, which is
                // document-global), then descend to flip the target node.
                materialize_binding_exposures(def);
                let static_slot = static_slot_for(def, &node_id, &inner_param);
                // D5 section seed: resolve the innermost enclosing group's
                // display name from the ROOT nodes + scope_path BEFORE
                // descend_level narrows the borrow to the target level (an
                // immutable read; the &mut borrow below starts only after
                // this value is fully owned).
                let inner_section = innermost_group_display_name(&def.nodes, &scope);
                let (nodes, _wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                let prev_in_set = flip_node_exposed(nodes, node_u32_id, &inner_param, expose)?;
                Some((prev_in_set, static_slot, inner_section))
            },
        )
        .flatten();

        let Some((prev_in_set, static_slot, inner_section)) = graph_result else {
            // Target / scope / node didn't resolve — nothing to undo.
            self.reverse = NodeExposeReverse::None;
            return;
        };

        // Mirror collapse: both effect and generator targets run through
        // `mirror_effect_side` over the target's `&mut PresetInstance` — a
        // bundled param flips its `param_values[slot].exposed`, a user-added
        // param appends/removes a binding (and prunes its automation) via the
        // kind-aware `append_user_binding` / `remove_user_binding_by_id`. A
        // generator's `param_values[].exposed` bool is unread (its card shows
        // every graph param), so the bundled-slot flip is a harmless no-op
        // there; the real exposure is the `exposed_params` set flipped above.
        let instance: Option<&mut manifold_core::effects::PresetInstance> = match &self.target {
            GraphTarget::Effect(effect_id) => project.find_effect_by_id_mut(effect_id),
            GraphTarget::Generator(layer_id) => project
                .timeline
                .find_layer_by_id_mut(layer_id)
                .map(|(_, layer)| layer.gen_params_or_init()),
        };
        let mirror = match instance {
            Some(inst) => mirror_effect_side(
                inst,
                &node_id,
                &node_handle,
                &inner_param,
                expose,
                static_slot,
                &inner_label,
                inner_min,
                inner_max,
                inner_default,
                inner_convert,
                inner_is_angle,
                &inner_value_labels,
                inner_section,
            ),
            // Instance vanished between the graph borrow and the mirror borrow.
            // Capture just the graph bit so undo restores it.
            None => EffectMirrorReverse::NoOp,
        };

        self.reverse = NodeExposeReverse::Captured {
            prev_in_set,
            mirror,
        };
    }

    fn undo(&mut self, project: &mut Project) {
        let reverse = std::mem::take(&mut self.reverse);
        let NodeExposeReverse::Captured {
            prev_in_set,
            mirror,
        } = reverse
        else {
            return;
        };

        let inner_param = self.inner_param.clone();

        // Mirror collapse: restore the target's `&mut PresetInstance` through
        // `unmirror_effect_side` (binding + slot + automation, all in one
        // borrow now that envelopes ride on the instance), then restore the
        // graph `exposed_params` bit. Identical for both kinds.
        let instance: Option<&mut manifold_core::effects::PresetInstance> = match &self.target {
            GraphTarget::Effect(effect_id) => project.find_effect_by_id_mut(effect_id),
            GraphTarget::Generator(layer_id) => project
                .timeline
                .find_layer_by_id_mut(layer_id)
                .map(|(_, layer)| layer.gen_params_or_init()),
        };
        if let Some(inst) = instance {
            unmirror_effect_side(inst, mirror);
        }
        let scope = self.scope_path.clone();
        let node_u32_id = self.node_u32_id;
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            if let Some((nodes, _wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope) {
                restore_node_exposed(nodes, node_u32_id, &inner_param, prev_in_set);
            }
        });
    }

    fn description(&self) -> &str {
        if self.expose {
            "Expose Param"
        } else {
            "Hide Param"
        }
    }
}

#[allow(clippy::too_many_arguments)]
/// Find the node `(node_id, node_handle)` addresses in `nodes` (recursing
/// into group bodies), using the same "empty stable id defaults to handle"
/// convention `ToggleNodeParamExposeCommand::execute` uses to mint the
/// identity in the first place: prefer a real `node_id` match; fall back to
/// `handle` only for a node whose own `node_id` is empty (a bundled node
/// that predates stable ids). D9's freeze-on-unmap write target.
fn find_node_by_id_or_handle_mut<'a>(
    nodes: &'a mut [EffectGraphNode],
    node_id: &NodeId,
    node_handle: &str,
) -> Option<&'a mut EffectGraphNode> {
    let idx = nodes.iter().position(|n| {
        (!n.node_id.is_empty() && &n.node_id == node_id)
            || (n.node_id.is_empty() && n.handle.as_deref() == Some(node_handle))
    });
    if let Some(idx) = idx {
        return Some(&mut nodes[idx]);
    }
    for n in nodes.iter_mut() {
        if let Some(group) = n.group.as_deref_mut()
            && let Some(found) = find_node_by_id_or_handle_mut(&mut group.nodes, node_id, node_handle)
        {
            return Some(found);
        }
    }
    None
}

/// Convert an effective f32 value to the `SerializedParamValue` shape its
/// `ParamConvert` implies — the def-slot write shape for D9's freeze, mirror
/// of `param_binding::convert_param_value` (which targets the renderer-side
/// `ParamValue` instead of the wire `SerializedParamValue`).
fn effective_value_to_serialized(
    convert: manifold_core::effects::ParamConvert,
    value: f32,
) -> SerializedParamValue {
    use manifold_core::effects::ParamConvert;
    match convert {
        ParamConvert::Float | ParamConvert::Trigger => SerializedParamValue::Float { value },
        ParamConvert::IntRound => SerializedParamValue::Int { value: value.round() as i32 },
        ParamConvert::BoolThreshold => SerializedParamValue::Bool { value: value > 0.5 },
        ParamConvert::EnumRound => SerializedParamValue::Enum {
            value: value.round().max(0.0) as u32,
        },
    }
}

fn mirror_effect_side(
    effect: &mut manifold_core::effects::PresetInstance,
    node_id: &NodeId,
    node_handle: &str,
    inner_param: &str,
    expose: bool,
    static_slot: Option<usize>,
    inner_label: &str,
    inner_min: f32,
    inner_max: f32,
    inner_default: f32,
    inner_convert: manifold_core::effects::ParamConvert,
    inner_is_angle: bool,
    inner_value_labels: &[String],
    // D5 (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §2): the innermost
    // enclosing group's display name, resolved by the caller from
    // `scope_path` BEFORE this fn runs. Only used on the append-new-binding
    // path (a static-slot toggle flips an EXISTING bundled spec, whose
    // section is whatever the preset author/importer set — expose never
    // overwrites it).
    inner_section: Option<String>,
) -> EffectMirrorReverse {
    use manifold_core::effects::UserParamBinding;

    if let Some(slot) = static_slot {
        // Bundled-prefix path: flip the exposure flag on the slot-th manifest
        // entry (bundled params occupy the prefix, in card order). Resolve the
        // positional slot to its stable id so undo re-addresses the same param.
        let Some(param_id) = effect.params.iter().nth(slot).map(|p| p.id().to_string())
        else {
            return EffectMirrorReverse::NoOp;
        };
        let prev_exposed = effect.is_param_exposed(&param_id);
        if prev_exposed == expose {
            return EffectMirrorReverse::NoOp;
        }
        effect.set_param_exposed(&param_id, expose);
        return EffectMirrorReverse::StaticSlot { param_id, prev_exposed };
    }

    // Non-static path: append / remove a user-added binding (stored in
    // the per-instance graph's `preset_metadata.bindings`).
    let user_bindings = effect.user_param_bindings();
    let existing_position = user_bindings
        .iter()
        .position(|b| &b.node_id == node_id && b.inner_param == inner_param);

    if expose {
        if existing_position.is_some() {
            return EffectMirrorReverse::NoOp;
        }
        let existing_ids: Vec<String> =
            user_bindings.iter().map(|b| b.id.clone()).collect();
        let id = crate::commands::effects::generate_user_param_id(
            node_handle,
            inner_param,
            &existing_ids,
        );
        let binding = UserParamBinding {
            id: id.clone(),
            label: inner_label.to_string(),
            node_id: node_id.clone(),
            legacy_node_handle: None,
            inner_param: inner_param.to_string(),
            min: inner_min,
            max: inner_max,
            default_value: inner_default,
            convert: inner_convert,
            is_angle: inner_is_angle,
            invert: false,
            curve: Default::default(),
            scale: 1.0,
            offset: 0.0,
            value_labels: inner_value_labels.to_vec(),
            section: inner_section,
        };
        effect.append_user_binding(binding);
        EffectMirrorReverse::AppendedUserBinding {
            user_param_id: id,
        }
    } else {
        let Some(position) = existing_position else {
            return EffectMirrorReverse::NoOp;
        };
        let binding = user_bindings[position].clone();
        let binding_id = binding.id.clone();
        // Capture the full manifest entry BEFORE removal so undo reinstates
        // the exact snapshot (value + base + calibration). The entry is
        // coupled to the binding by id (append/remove keep them in lockstep),
        // so there is no positional slot to compute — a generator instance
        // routes through here too (mirror collapse).
        let param = effect
            .params
            .get(&binding_id)
            .cloned()
            .expect("manifest entry present for a live user binding");
        // Prune any effect-local automation that referenced this
        // binding's id. After removal the id stops resolving anywhere
        // and the rows would silently never apply — capture them on
        // the reverse state so undo restores both the binding AND the
        // automation it carried.
        let removed_drivers = if let Some(ds) = effect.drivers.as_mut() {
            let mut taken = Vec::new();
            ds.retain(|d| {
                let keep = d.param_id != binding_id;
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
        let removed_ableton_mappings = if let Some(ms) = effect.ableton_mappings.as_mut() {
            let mut taken = Vec::new();
            ms.retain(|m| {
                let keep = m.param_id != binding_id;
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
        let removed_envelopes = if let Some(es) = effect.envelopes.as_mut() {
            let mut taken = Vec::new();
            es.retain(|e| {
                let keep = e.param_id != binding_id;
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
        // D9 (`docs/PARAM_TWO_WAY_BINDING_DESIGN.md`): freeze the EFFECTIVE
        // value into the def slot this binding stops governing, so unmapping
        // never visually snaps the render to whatever stale value the slot
        // held from a pre-binding write. Must run BEFORE the binding is
        // removed (needs `binding`'s reshape) but after `param` is captured
        // above (needs its live `.value`).
        if let Some(graph) = effect.graph.as_mut() {
            let effective = manifold_core::effects::apply_card_reshape(
                param.value,
                binding.min,
                binding.max,
                binding.invert,
                binding.curve,
                binding.scale,
                binding.offset,
            );
            if let Some(target_node) =
                find_node_by_id_or_handle_mut(&mut graph.nodes, node_id, node_handle)
            {
                target_node.params.insert(
                    inner_param.to_string(),
                    effective_value_to_serialized(binding.convert, effective),
                );
            }
        }
        let _ = effect.remove_user_binding_by_id(&binding_id);
        EffectMirrorReverse::RemovedUserBinding {
            binding,
            position,
            param,
            removed_drivers,
            removed_ableton_mappings,
            removed_envelopes,
        }
    }
}

fn unmirror_effect_side(
    effect: &mut manifold_core::effects::PresetInstance,
    mirror: EffectMirrorReverse,
) {
    match mirror {
        EffectMirrorReverse::NoOp => {}
        EffectMirrorReverse::StaticSlot { param_id, prev_exposed } => {
            effect.set_param_exposed(&param_id, prev_exposed);
        }
        EffectMirrorReverse::AppendedUserBinding { user_param_id } => {
            let _ = effect.remove_user_binding_by_id(&user_param_id);
        }
        EffectMirrorReverse::RemovedUserBinding {
            binding,
            position,
            param,
            removed_drivers,
            removed_ableton_mappings,
            removed_envelopes,
        } => {
            // Restore the binding (graph metadata + reshape note) and its
            // manifest entry at the original tail position so other user
            // bindings keep their card order.
            effect.restore_user_binding_at(binding, position, param);
            // Restore the automation rows that referenced this binding.
            // The same id resolves through the manifest again since we
            // re-inserted the binding above.
            if !removed_drivers.is_empty() {
                effect
                    .drivers
                    .get_or_insert_with(Vec::new)
                    .extend(removed_drivers);
            }
            if !removed_ableton_mappings.is_empty() {
                effect
                    .ableton_mappings
                    .get_or_insert_with(Vec::new)
                    .extend(removed_ableton_mappings);
            }
            if !removed_envelopes.is_empty() {
                effect
                    .envelopes
                    .get_or_insert_with(Vec::new)
                    .extend(removed_envelopes);
            }
        }
    }
}


#[cfg(test)]
mod tests {
    use super::super::*;
    use super::super::test_support::*;
    use manifold_core::EffectId;
    use manifold_core::PresetTypeId;
    use manifold_core::effects::PresetInstance;
    use crate::command::Command;

    #[test]
    fn toggle_node_param_expose_against_generator_flips_graph_exposed_set() {
        let (mut project, lid) = project_with_one_generator_layer();
        let mut cmd = ToggleNodeParamExposeCommand::new(
            GraphTarget::Generator(lid.clone()),
            manifold_core::NodeId::new("uv_transform"),
            1,
            "uv_transform".to_string(),
            "rotation".to_string(),
            true,
            mirror_catalog_default(),
            "Rotation".to_string(),
            -180.0,
            180.0,
            0.0,
            manifold_core::effects::ParamConvert::Float,
            false,
            Vec::new(),
        );

        cmd.execute(&mut project);

        let (_, layer) = project.timeline.find_layer_by_id(&lid).unwrap();
        let def = layer.generator_graph().unwrap();
        let node = def
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("uv_transform"))
            .unwrap();
        assert!(
            node.exposed_params.contains("rotation"),
            "expose flips the graph exposed_params set"
        );

        // Undo flips it back.
        cmd.undo(&mut project);
        let (_, layer) = project.timeline.find_layer_by_id(&lid).unwrap();
        let def = layer.generator_graph().unwrap();
        let node = def
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("uv_transform"))
            .unwrap();
        assert!(
            !node.exposed_params.contains("rotation"),
            "undo restores prior exposed_params state"
        );
    }

    /// Regression (the on-node expose checkbox bug): exposing a param on a node
    /// *nested inside a group* — addressed the way the canvas actually addresses
    /// it, by `(scope_path, node_u32_id)` with an EMPTY stable `node_id` (bundled
    /// nodes ship empty) — must flip `exposed_params` on that nested node, NOT a
    /// top-level one. The old command scanned only the document root and matched
    /// by the empty `node_id`, so it hit the wrong node (or none): the checkbox
    /// never reflected the state and couldn't be unchecked. It must also mint the
    /// card binding with `node_id` defaulted to the handle — the same convention
    /// the runtime graph loader uses — so the slider actually drives the param.
    #[test]
    fn exposing_a_nested_node_param_targets_the_body_node_and_binds_by_handle() {
        let (mut project, fx) = project_with_graph(mirror_catalog_default());

        // Collapse `uv_transform` (doc id 1, empty stable node_id) into a group.
        let mut group = GroupNodesCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            vec![1],
            "g".to_string(),
            (0.0, 0.0),
            mirror_catalog_default(),
        );
        group.execute(&mut project);
        let g_id = graph_of(&project, &fx)
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("g"))
            .unwrap()
            .id;

        // Expose `rotation` exactly as the canvas would: empty stable node_id,
        // located by u32 doc id 1 at scope [g_id].
        let mut expose = ToggleNodeParamExposeCommand::new(
            GraphTarget::Effect(fx.clone()),
            manifold_core::NodeId::default(),
            1,
            "uv_transform".to_string(),
            "rotation".to_string(),
            true,
            mirror_catalog_default(),
            "Rotation".to_string(),
            -180.0,
            180.0,
            0.0,
            manifold_core::effects::ParamConvert::Float,
            false,
            Vec::new(),
        )
        .with_scope(vec![g_id]);
        expose.execute(&mut project);

        // The NESTED node carries the exposure.
        let body_has_rotation = |project: &Project| {
            graph_of(project, &fx)
                .nodes
                .iter()
                .find(|n| n.id == g_id)
                .unwrap()
                .group
                .as_deref()
                .unwrap()
                .nodes
                .iter()
                .find(|n| n.handle.as_deref() == Some("uv_transform"))
                .unwrap()
                .exposed_params
                .contains("rotation")
        };
        assert!(
            body_has_rotation(&project),
            "expose flipped the nested body node's exposed_params"
        );
        // No ROOT node absorbed it (the old empty-node_id top-level scan bug).
        assert!(
            graph_of(&project, &fx)
                .nodes
                .iter()
                .all(|n| !n.exposed_params.contains("rotation")),
            "no top-level node was wrongly exposed"
        );

        // The card binding targets the handle-defaulted id, so it resolves to
        // the runtime node (`graph_loader` applies the same default) — not a
        // dead empty-id binding.
        let fx_inst = project.find_effect_by_id(&fx).unwrap();
        let ub = fx_inst.user_param_bindings();
        assert_eq!(ub.len(), 1, "one user binding minted");
        assert_eq!(
            ub[0].node_id, "uv_transform",
            "binding node_id defaults to the handle"
        );

        // Undo clears the nested exposure.
        expose.undo(&mut project);
        assert!(
            !body_has_rotation(&project),
            "undo restored the nested node's exposed_params"
        );
    }

    #[test]
    fn exposing_inside_a_group_stamps_section_from_the_group_name() {
        let (mut project, fx) = project_with_graph(mirror_catalog_default());

        let mut group = GroupNodesCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            vec![1],
            "g".to_string(),
            (0.0, 0.0),
            mirror_catalog_default(),
        );
        group.execute(&mut project);
        let g_id = graph_of(&project, &fx)
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("g"))
            .unwrap()
            .id;

        let mut expose = ToggleNodeParamExposeCommand::new(
            GraphTarget::Effect(fx.clone()),
            manifold_core::NodeId::default(),
            1,
            "uv_transform".to_string(),
            "rotation".to_string(),
            true,
            mirror_catalog_default(),
            "Rotation".to_string(),
            -180.0,
            180.0,
            0.0,
            manifold_core::effects::ParamConvert::Float,
            false,
            Vec::new(),
        )
        .with_scope(vec![g_id]);
        expose.execute(&mut project);

        let fx_inst = project.find_effect_by_id(&fx).unwrap();
        let ub = fx_inst.user_param_bindings();
        assert_eq!(ub.len(), 1);
        let entry = fx_inst.params.get(&ub[0].id).expect("manifest entry for the new binding");
        assert_eq!(
            entry.spec.section.as_deref(),
            Some("g"),
            "expose-time seeding stamps the innermost enclosing group's display name"
        );

        // Undo removes the whole binding (spec + section together) — no
        // dangling manifest entry.
        expose.undo(&mut project);
        let fx_inst = project.find_effect_by_id(&fx).unwrap();
        assert!(fx_inst.params.get(&ub[0].id).is_none(), "undo removed the manifest entry entirely");
    }

    #[test]
    fn exposing_at_top_level_leaves_section_none() {
        let (mut project, fx) = project_with_graph(mirror_catalog_default());

        // No grouping — expose `rotation` directly at the document root
        // (empty scope_path).
        let mut expose = ToggleNodeParamExposeCommand::new(
            GraphTarget::Effect(fx.clone()),
            manifold_core::NodeId::default(),
            1,
            "uv_transform".to_string(),
            "rotation".to_string(),
            true,
            mirror_catalog_default(),
            "Rotation".to_string(),
            -180.0,
            180.0,
            0.0,
            manifold_core::effects::ParamConvert::Float,
            false,
            Vec::new(),
        );
        expose.execute(&mut project);

        let fx_inst = project.find_effect_by_id(&fx).unwrap();
        let ub = fx_inst.user_param_bindings();
        assert_eq!(ub.len(), 1);
        let entry = fx_inst.params.get(&ub[0].id).unwrap();
        assert_eq!(entry.spec.section, None, "a top-level expose gets no section");
    }

    #[test]
    fn exposing_survives_json_round_trip_with_section() {
        let (mut project, fx) = project_with_graph(mirror_catalog_default());
        let mut group = GroupNodesCommand::new(
            GraphTarget::Effect(fx.clone()),
            vec![],
            vec![1],
            "g".to_string(),
            (0.0, 0.0),
            mirror_catalog_default(),
        );
        group.execute(&mut project);
        let g_id = graph_of(&project, &fx)
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("g"))
            .unwrap()
            .id;
        let mut expose = ToggleNodeParamExposeCommand::new(
            GraphTarget::Effect(fx.clone()),
            manifold_core::NodeId::default(),
            1,
            "uv_transform".to_string(),
            "rotation".to_string(),
            true,
            mirror_catalog_default(),
            "Rotation".to_string(),
            -180.0,
            180.0,
            0.0,
            manifold_core::effects::ParamConvert::Float,
            false,
            Vec::new(),
        )
        .with_scope(vec![g_id]);
        expose.execute(&mut project);

        let fx_inst = project.find_effect_by_id(&fx).unwrap();
        let ub_id = fx_inst.user_param_bindings()[0].id.clone();

        // "save" — serialize the instance (this is what the project save
        // path emits per effect; PARAM_STORAGE_BOUNDARIES_DESIGN D12 derives
        // `graph.preset_metadata.params` from the live manifest here).
        let json = serde_json::to_string(fx_inst).unwrap();
        // "reload"
        let back: manifold_core::effects::PresetInstance = serde_json::from_str(&json).unwrap();
        let spec = back
            .graph
            .as_ref()
            .unwrap()
            .preset_metadata
            .as_ref()
            .unwrap()
            .params
            .iter()
            .find(|p| p.id == ub_id)
            .expect("the exposed param's spec survived the round trip");
        assert_eq!(
            spec.section.as_deref(),
            Some("g"),
            "the card row is still sectioned after save -> reload"
        );
    }

    #[test]
    fn exposing_a_non_preset_param_on_generator_appends_user_binding_and_grows_param_values() {
        // Regression: clicking the expose checkbox on a generator's
        // inner-node param that has NO preset binding (e.g.
        // `node.draw_lines:animate` on the Wireframe preset) must
        // synthesize a user-added BindingDef + ParamSpecDef in the
        // graph's preset_metadata AND extend gp.param_values by one
        // slot so the outer card has somewhere to render it.
        use manifold_core::effect_graph_def::{
            BindingDef, BindingTarget, EFFECT_GRAPH_VERSION_WITH_METADATA, ParamSpecDef,
            PresetMetadata,
        };
        use manifold_core::effects::ParamConvert;
        use manifold_core::preset_type_id::PresetTypeId;

        // Wireframe-like preset: two bundled bindings ("shape" → render.shape,
        // "scale" → render.scale) plus an inner node `render` whose
        // `animate` param is NOT bound. param_values has two bundled
        // slots.
        let preset_def = || EffectGraphDef {
            version: EFFECT_GRAPH_VERSION_WITH_METADATA,
            name: Some("wireframe-like".into()),
            description: None,
            preset_metadata: Some(PresetMetadata {
                id: PresetTypeId::new("test.wireframe"),
                display_name: "Wireframe".into(),
                category: "Procedural".into(),
                osc_prefix: "wireframe".into(),
                legacy_discriminant: None,
                available: true,
                is_line_based: false,
                params: vec![
                    ParamSpecDef {
                        id: "shape".into(),
                        name: "Shape".into(),
                        min: 0.0,
                        max: 4.0,
                        default_value: 0.0,
                        whole_numbers: true,
                        is_toggle: false,
                        is_trigger: false,
                        value_labels: vec![],
                        format_string: None,
                        osc_suffix: String::new(),
                        curve: Default::default(),
                        invert: false,
                        is_angle: false,
                        is_trigger_gate: false,
                        wraps: false,
                        section: None,
                    },
                    ParamSpecDef {
                        id: "scale".into(),
                        name: "Scale".into(),
                        min: 0.25,
                        max: 3.0,
                        default_value: 1.0,
                        whole_numbers: false,
                        is_toggle: false,
                        is_trigger: false,
                        value_labels: vec![],
                        format_string: None,
                        osc_suffix: String::new(),
                        curve: Default::default(),
                        is_angle: false,
                        invert: false,
                        is_trigger_gate: false,
                        wraps: false,
                        section: None,
                    },
                ],
                bindings: vec![
                    BindingDef {
                        id: "shape".into(),
                        label: "Shape".into(),
                        default_value: 0.0,
                        target: BindingTarget::Node {
                            node_id: manifold_core::NodeId::new("render"),
                            param: "shape".into(),
                        },
                        convert: ParamConvert::EnumRound,
                        user_added: false,
                        scale: 1.0,
                        offset: 0.0,
                    },
                    BindingDef {
                        id: "scale".into(),
                        label: "Scale".into(),
                        default_value: 1.0,
                        target: BindingTarget::Node {
                            node_id: manifold_core::NodeId::new("render"),
                            param: "scale".into(),
                        },
                        convert: ParamConvert::Float,
                        user_added: false,
                        scale: 1.0,
                        offset: 0.0,
                    },
                ],
                skip_mode: Default::default(),
                param_aliases: vec![],
                value_aliases: vec![],
                string_params: vec![],
                string_bindings: vec![],
            }),
            nodes: vec![EffectGraphNode {
                id: 0,
                node_id: manifold_core::NodeId::new("render"),
                type_id: "node.draw_lines".to_string(),
                handle: Some("render".to_string()),
                params: BTreeMap::new(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            }],
            wires: vec![],
        };

        let (mut project, lid) = project_with_one_generator_layer();
        {
            let (_, layer) = project.timeline.find_layer_by_id_mut(&lid).unwrap();
            layer.gen_params_or_init().graph = Some(preset_def());
            // gen_params starts with the two bundled slot values.
            let gp = layer.gen_params_or_init();
            gp.init_defaults_for_type(PresetTypeId::from_string(
                "test.wireframe".to_string(),
            ));
            // Override values after init — the registry doesn't know
            // about our synthetic preset, so init may leave the vec
            // empty. Force the bundled slot count to match the preset.
            gp.params = manifold_core::params::ParamManifest::from_params(vec![
                slot("shape", 0.0, true),
                slot("scale", 1.0, true),
            ]);
            // slot() seeds base = value; mark base tracked (fork #16).
            gp.base_tracked = true;
        }

        // Expose `render.animate` — has no preset binding, so the
        // command must synthesize a user-added entry.
        let mut expose = ToggleNodeParamExposeCommand::new(
            GraphTarget::Generator(lid.clone()),
            manifold_core::NodeId::new("render"),
            0,
            "render".to_string(),
            "animate".to_string(),
            true,
            preset_def(),
            "Animate".to_string(),
            0.0,
            1.0,
            0.0,
            ParamConvert::BoolThreshold,
            false,
            Vec::new(),
        );
        expose.execute(&mut project);

        // Assert: preset_metadata grew by one entry in both lists,
        // marked user_added=true.
        let (_, layer) = project.timeline.find_layer_by_id(&lid).unwrap();
        let def = layer.generator_graph().unwrap();
        let meta = def.preset_metadata.as_ref().unwrap();
        assert_eq!(
            meta.params.len(),
            3,
            "preset_metadata.params grew by one user-added entry"
        );
        assert_eq!(
            meta.bindings.len(),
            3,
            "preset_metadata.bindings grew by one user-added entry"
        );
        let new_binding = meta.bindings.last().unwrap();
        assert!(
            new_binding.user_added,
            "newly appended binding is flagged user_added=true"
        );
        assert!(
            matches!(
                &new_binding.target,
                BindingTarget::Node { node_id, param }
                    if node_id == "render" && param == "animate"
            ),
            "new binding routes to render.animate"
        );

        // The id should be auto-generated; capture for later
        // assertions on undo.
        let user_param_id = new_binding.id.clone();
        assert!(
            user_param_id.starts_with("user.render.animate."),
            "id follows the user.<handle>.<param>.<n> convention, got `{user_param_id}`"
        );

        // gp.params grew by one to match.
        let gp = layer.gen_params().unwrap();
        assert_eq!(
            gp.params.len(),
            3,
            "params grew by one slot for the user-added binding"
        );

        // exposed_params on the render node now contains "animate".
        let render_node = def
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("render"))
            .unwrap();
        assert!(
            render_node.exposed_params.contains("animate"),
            "render.animate is now in exposed_params"
        );

        // Undo restores everything.
        expose.undo(&mut project);
        let (_, layer) = project.timeline.find_layer_by_id(&lid).unwrap();
        let def = layer.generator_graph().unwrap();
        let meta = def.preset_metadata.as_ref().unwrap();
        assert_eq!(meta.params.len(), 2, "undo removes the user-added param");
        assert_eq!(
            meta.bindings.len(),
            2,
            "undo removes the user-added binding"
        );
        let gp = layer.gen_params().unwrap();
        assert_eq!(gp.params.len(), 2, "undo pops the user-added slot");
        let render_node = def
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("render"))
            .unwrap();
        assert!(
            !render_node.exposed_params.contains("animate"),
            "undo restores exposed_params"
        );

        // Re-execute → state matches post-execute.
        expose.execute(&mut project);
        let (_, layer) = project.timeline.find_layer_by_id(&lid).unwrap();
        let def = layer.generator_graph().unwrap();
        let meta = def.preset_metadata.as_ref().unwrap();
        assert_eq!(meta.bindings.len(), 3);
        assert_eq!(meta.bindings.last().unwrap().id, user_param_id);

        // user_added flag survives JSON round-trip.
        let json = serde_json::to_string(def).unwrap();
        let reloaded: EffectGraphDef = serde_json::from_str(&json).unwrap();
        let reloaded_meta = reloaded.preset_metadata.as_ref().unwrap();
        assert_eq!(reloaded_meta.bindings.len(), 3);
        assert!(
            reloaded_meta.bindings.last().unwrap().user_added,
            "user_added=true survives serialization"
        );
        // Bundled bindings serialize without the field set; on
        // deserialize the default `false` should kick in.
        assert!(
            !reloaded_meta.bindings[0].user_added,
            "bundled binding stays user_added=false after round-trip"
        );
    }

    #[test]
    fn unexposing_a_user_added_generator_binding_removes_metadata_and_shrinks_param_values() {
        // The inverse of the test above: unexpose a previously
        // user-added binding. Removes the metadata + slot + any
        // referencing automation (drivers / envelopes / Ableton),
        // captures for undo.
        use manifold_core::effect_graph_def::{
            BindingDef, BindingTarget, EFFECT_GRAPH_VERSION_WITH_METADATA, ParamSpecDef,
            PresetMetadata,
        };
        use manifold_core::effects::{ParamConvert, ParamEnvelope, ParameterDriver};
        use manifold_core::preset_type_id::PresetTypeId;
        use manifold_core::types::{BeatDivision, DriverWaveform};

        // Preset already carries a user-added binding (simulates
        // "user-added in a prior session, now loaded from a save
        // file"). One bundled binding + one user-added binding.
        let preset_def_with_user_added = || EffectGraphDef {
            version: EFFECT_GRAPH_VERSION_WITH_METADATA,
            name: Some("wireframe-like".into()),
            description: None,
            preset_metadata: Some(PresetMetadata {
                id: PresetTypeId::new("test.wireframe"),
                display_name: "Wireframe".into(),
                category: "Procedural".into(),
                osc_prefix: "wireframe".into(),
                legacy_discriminant: None,
                available: true,
                is_line_based: false,
                params: vec![
                    ParamSpecDef {
                        id: "shape".into(),
                        name: "Shape".into(),
                        min: 0.0,
                        max: 4.0,
                        default_value: 0.0,
                        whole_numbers: true,
                        is_toggle: false,
                        is_trigger: false,
                        value_labels: vec![],
                        format_string: None,
                        osc_suffix: String::new(),
                        curve: Default::default(),
                        invert: false,
                        is_angle: false,
                        is_trigger_gate: false,
                        wraps: false,
                        section: None,
                    },
                    ParamSpecDef {
                        id: "user.render.animate.1".into(),
                        name: "Animate".into(),
                        min: 0.0,
                        max: 1.0,
                        default_value: 0.0,
                        whole_numbers: false,
                        is_toggle: true,
                        is_trigger: false,
                        value_labels: vec![],
                        format_string: None,
                        osc_suffix: String::new(),
                        curve: Default::default(),
                        invert: false,
                        is_angle: false,
                        is_trigger_gate: false,
                        wraps: false,
                        section: None,
                    },
                ],
                bindings: vec![
                    BindingDef {
                        id: "shape".into(),
                        label: "Shape".into(),
                        default_value: 0.0,
                        target: BindingTarget::Node {
                            node_id: manifold_core::NodeId::new("render"),
                            param: "shape".into(),
                        },
                        convert: ParamConvert::EnumRound,
                        user_added: false,
                        scale: 1.0,
                        offset: 0.0,
                    },
                    BindingDef {
                        id: "user.render.animate.1".into(),
                        label: "Animate".into(),
                        default_value: 0.0,
                        target: BindingTarget::Node {
                            node_id: manifold_core::NodeId::new("render"),
                            param: "animate".into(),
                        },
                        convert: ParamConvert::BoolThreshold,
                        user_added: true,
                        scale: 1.0,
                        offset: 0.0,
                    },
                ],
                skip_mode: Default::default(),
                param_aliases: vec![],
                value_aliases: vec![],
                string_params: vec![],
                string_bindings: vec![],
            }),
            nodes: vec![EffectGraphNode {
                id: 0,
                node_id: manifold_core::NodeId::new("render"),
                type_id: "node.draw_lines".to_string(),
                handle: Some("render".to_string()),
                params: BTreeMap::new(),
                exposed_params: {
                    let mut s = std::collections::BTreeSet::new();
                    s.insert("animate".to_string());
                    s
                },
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            }],
            wires: vec![],
        };

        let (mut project, lid) = project_with_one_generator_layer();
        {
            let (_, layer) = project.timeline.find_layer_by_id_mut(&lid).unwrap();
            layer.gen_params_or_init().graph = Some(preset_def_with_user_added());
            let gp = layer.gen_params_or_init();
            gp.init_defaults_for_type(PresetTypeId::from_string(
                "test.wireframe".to_string(),
            ));
            gp.params = manifold_core::params::ParamManifest::from_params(vec![
                slot("shape", 0.0, true),
                slot("user.render.animate.1", 0.75, true),
            ]); // bundled `shape` + user-added `animate`
            gp.base_tracked = true;
            // Attach a driver + envelope on the user-added id — they
            // should get pruned on unexpose and restored on undo.
            gp.drivers = Some(vec![ParameterDriver {
                param_id: std::borrow::Cow::Owned("user.render.animate.1".to_string()),
                beat_division: BeatDivision::Quarter,
                waveform: DriverWaveform::Sine,
                enabled: true,
                phase: 0.0,
                base_value: 0.5,
                trim_min: 0.0,
                trim_max: 1.0,
                reversed: false,
                free_period_beats: None,
                legacy_param_index: None,
                is_paused_by_user: false,
            }]);
            gp.envelopes = Some(vec![ParamEnvelope::new(std::borrow::Cow::Owned(
                "user.render.animate.1".to_string(),
            ))]);
        }

        let mut unexpose = ToggleNodeParamExposeCommand::new(
            GraphTarget::Generator(lid.clone()),
            manifold_core::NodeId::new("render"),
            0,
            "render".to_string(),
            "animate".to_string(),
            false,
            preset_def_with_user_added(),
            "Animate".to_string(),
            0.0,
            1.0,
            0.0,
            ParamConvert::BoolThreshold,
            false,
            Vec::new(),
        );
        unexpose.execute(&mut project);

        let (_, layer) = project.timeline.find_layer_by_id(&lid).unwrap();
        let def = layer.generator_graph().unwrap();
        let meta = def.preset_metadata.as_ref().unwrap();
        assert_eq!(meta.params.len(), 1, "user-added param removed");
        assert_eq!(meta.bindings.len(), 1, "user-added binding removed");
        assert_eq!(meta.bindings[0].id, "shape", "bundled binding survives");

        let gp = layer.gen_params().unwrap();
        assert_eq!(gp.params.len(), 1, "user-added slot removed");
        assert_eq!(
            gp.params.get("shape").unwrap().value,
            0.0,
            "bundled `shape` value intact"
        );
        assert!(
            gp.drivers.is_none() || gp.drivers.as_ref().unwrap().is_empty(),
            "driver referencing user-added id pruned"
        );
        assert!(
            gp.envelopes.is_none() || gp.envelopes.as_ref().unwrap().is_empty(),
            "envelope referencing user-added id pruned"
        );

        // Undo restores everything.
        unexpose.undo(&mut project);
        let (_, layer) = project.timeline.find_layer_by_id(&lid).unwrap();
        let def = layer.generator_graph().unwrap();
        let meta = def.preset_metadata.as_ref().unwrap();
        assert_eq!(meta.params.len(), 2, "undo restores user-added param");
        assert_eq!(meta.bindings.len(), 2, "undo restores user-added binding");
        assert_eq!(meta.bindings[1].id, "user.render.animate.1");
        assert!(meta.bindings[1].user_added);

        let gp = layer.gen_params().unwrap();
        assert_eq!(gp.params.len(), 2, "undo restores the slot");
        assert!(
            (gp.params.get("user.render.animate.1").unwrap().value - 0.75).abs() < f32::EPSILON,
            "slot value (0.75) restored"
        );
        assert_eq!(
            gp.drivers.as_ref().map(|d| d.len()).unwrap_or(0),
            1,
            "driver restored"
        );
        assert_eq!(
            gp.envelopes.as_ref().map(|e| e.len()).unwrap_or(0),
            1,
            "envelope restored"
        );
    }

    #[test]
    fn unexposing_a_user_binding_prunes_and_restores_orphan_automation() {
        // When the user un-checks a non-preset-bound exposure on an
        // effect (i.e. it was previously exposed via a UserParamBinding),
        // any drivers / Ableton mappings that referenced the binding's
        // param_id would otherwise become orphans — still in the
        // project file, never matched at resolve time. The unified
        // command prunes them on unexpose and restores them on undo.
        use manifold_core::ableton_mapping::{
            AbletonDeviceIdentity, AbletonMacroAddress, AbletonMappingStatus,
            AbletonParamMapping,
        };
        use manifold_core::effects::{ParamConvert, ParameterDriver};
        use manifold_core::types::{BeatDivision, DriverWaveform};

        // Set up an effect with one user-exposed inner param + driver
        // + Ableton mapping that target its synthesised id.
        let mut project = Project::default();
        let effect_id = EffectId::new("orphan-cleanup-test");
        let mut fx = PresetInstance::new(PresetTypeId::new("test.mirror"));
        fx.id = effect_id.clone();
        project.settings.master_effects.push(fx);

        // Expose first.
        let mut expose = ToggleNodeParamExposeCommand::new(
            GraphTarget::Effect(effect_id.clone()),
            manifold_core::NodeId::new("uv_transform"),
            1,
            "uv_transform".to_string(),
            "rotation".to_string(),
            true,
            mirror_catalog_default(),
            "Rotation".to_string(),
            -180.0,
            180.0,
            0.0,
            ParamConvert::Float,
            false,
            Vec::new(),
        );
        expose.execute(&mut project);

        // Now attach a driver + ableton mapping to the synthesised
        // user_param_id.
        let user_param_id = {
            let fx = project.find_effect_by_id(&effect_id).unwrap();
            let ub = fx.user_param_bindings();
            assert_eq!(ub.len(), 1);
            ub[0].id.clone()
        };
        {
            let fx = project.find_effect_by_id_mut(&effect_id).unwrap();
            fx.drivers = Some(vec![ParameterDriver {
                param_id: std::borrow::Cow::Owned(user_param_id.clone()),
                beat_division: BeatDivision::Quarter,
                waveform: DriverWaveform::Sine,
                enabled: true,
                phase: 0.0,
                base_value: 0.5,
                trim_min: 0.0,
                trim_max: 1.0,
                reversed: false,
                free_period_beats: None,
                legacy_param_index: None,
                is_paused_by_user: false,
            }]);
            fx.ableton_mappings = Some(vec![AbletonParamMapping {
                param_id: std::borrow::Cow::Owned(user_param_id.clone()),
                address: AbletonMacroAddress {
                    track_id: 0,
                    device_id: 0,
                    param_id: 0,
                    device_identity: AbletonDeviceIdentity {
                        device_class_name: "InstrumentGroupDevice".into(),
                    },
                    track_name: "Master".into(),
                    device_name: "Manifold".into(),
                    macro_name: "Macro 1".into(),
                },
                range_min: 0.0,
                range_max: 1.0,
                inverted: false,
                legacy_param_index: None,
                last_value: 0.0,
                status: AbletonMappingStatus::Active,
            }]);
        }

        // Unexpose. Drivers + Ableton mappings must be pruned.
        let mut unexpose = ToggleNodeParamExposeCommand::new(
            GraphTarget::Effect(effect_id.clone()),
            manifold_core::NodeId::new("uv_transform"),
            1,
            "uv_transform".to_string(),
            "rotation".to_string(),
            false,
            mirror_catalog_default(),
            "Rotation".to_string(),
            -180.0,
            180.0,
            0.0,
            ParamConvert::Float,
            false,
            Vec::new(),
        );
        unexpose.execute(&mut project);

        let fx = project.find_effect_by_id(&effect_id).unwrap();
        assert!(
            fx.drivers.is_none() || fx.drivers.as_ref().unwrap().is_empty(),
            "drivers pruned on unexpose"
        );
        assert!(
            fx.ableton_mappings.is_none()
                || fx.ableton_mappings.as_ref().unwrap().is_empty(),
            "ableton_mappings pruned on unexpose"
        );

        // Undo restores both.
        unexpose.undo(&mut project);
        let fx = project.find_effect_by_id(&effect_id).unwrap();
        assert_eq!(fx.user_param_bindings().len(), 1, "binding restored");
        assert_eq!(
            fx.drivers.as_ref().map(|d| d.len()).unwrap_or(0),
            1,
            "driver restored"
        );
        assert_eq!(
            fx.ableton_mappings.as_ref().map(|m| m.len()).unwrap_or(0),
            1,
            "ableton mapping restored"
        );
        assert_eq!(
            fx.drivers.as_ref().unwrap()[0].param_id,
            std::borrow::Cow::<'static, str>::Owned(user_param_id.clone()),
        );
    }

    #[test]
    fn unexposing_a_user_binding_on_layer_effect_prunes_and_restores_envelopes() {
        // Same shape as the driver/Ableton orphan-cleanup test, for
        // envelopes — which since envelope-home unification live on the
        // effect instance. Unexpose prunes envelopes matching the
        // binding's param_id (in the same borrow as drivers/Ableton) and
        // restores them on undo.
        use manifold_core::effects::{ParamConvert, ParamEnvelope};
        use manifold_core::layer::Layer;
        use manifold_core::types::LayerType;

        let effect_type = PresetTypeId::new("test.mirror");
        let effect_id = EffectId::new("envelope-cleanup-test");

        let mut project = Project::default();
        let mut layer = Layer::new("Test".to_string(), LayerType::Generator, 0);
        let mut fx = PresetInstance::new(effect_type.clone());
        fx.id = effect_id.clone();
        layer.effects = Some(vec![fx]);
        project.timeline.layers.push(layer);

        // Expose first, attach an envelope to the synthesised id.
        let mut expose = ToggleNodeParamExposeCommand::new(
            GraphTarget::Effect(effect_id.clone()),
            manifold_core::NodeId::new("uv_transform"),
            1,
            "uv_transform".to_string(),
            "rotation".to_string(),
            true,
            mirror_catalog_default(),
            "Rotation".to_string(),
            -180.0,
            180.0,
            0.0,
            ParamConvert::Float,
            false,
            Vec::new(),
        );
        expose.execute(&mut project);

        let user_param_id = {
            let fx = project.find_effect_by_id(&effect_id).unwrap();
            fx.user_param_bindings()[0].id.clone()
        };
        {
            let fx = project.find_effect_by_id_mut(&effect_id).unwrap();
            fx.envelopes_mut().push(ParamEnvelope::new(user_param_id.clone()));
            // Add an unrelated envelope that should NOT get pruned —
            // different param_id.
            fx.envelopes_mut().push(ParamEnvelope::new("unrelated.param".to_string()));
        }

        // Unexpose. The matching envelope must be pruned; the unrelated
        // one must survive.
        let mut unexpose = ToggleNodeParamExposeCommand::new(
            GraphTarget::Effect(effect_id.clone()),
            manifold_core::NodeId::new("uv_transform"),
            1,
            "uv_transform".to_string(),
            "rotation".to_string(),
            false,
            mirror_catalog_default(),
            "Rotation".to_string(),
            -180.0,
            180.0,
            0.0,
            ParamConvert::Float,
            false,
            Vec::new(),
        );
        unexpose.execute(&mut project);

        let fx = project.find_effect_by_id(&effect_id).unwrap();
        let envs = fx.envelopes.as_deref().unwrap_or(&[]);
        assert_eq!(envs.len(), 1, "matching envelope pruned, unrelated kept");
        assert_eq!(envs[0].param_id, "unrelated.param");

        // Undo restores the pruned envelope alongside the binding.
        unexpose.undo(&mut project);
        let fx = project.find_effect_by_id(&effect_id).unwrap();
        let envs = fx.envelopes.as_deref().unwrap_or(&[]);
        assert_eq!(envs.len(), 2, "matching envelope restored");
        assert!(
            envs.iter().any(|e| e.param_id == user_param_id),
            "restored envelope points back at the binding's id"
        );
    }

    #[test]
    fn unchecking_a_preset_bound_param_sticks_across_persistence() {
        // Regression: when the user UNCHECKS a preset-bound param,
        // the next snapshot must reflect the uncheck. Previously the
        // `into_graph` binding backfill ran unconditionally and
        // re-set the exposure, masking the user's intent.
        use manifold_core::effect_graph_def::{
            BindingDef, BindingTarget, ParamSpecDef, PresetMetadata,
            EFFECT_GRAPH_VERSION_WITH_METADATA,
        };
        use manifold_core::effects::ParamConvert;

        // Build a tiny preset def: one node (`gen` with a `pattern`
        // param) with a single binding (outer "Pattern" → gen.pattern).
        let preset_def_with_pattern_binding = || EffectGraphDef {
            version: EFFECT_GRAPH_VERSION_WITH_METADATA,
            name: Some("test-preset".into()),
            description: None,
            preset_metadata: Some(PresetMetadata {
                id: PresetTypeId::new("test.plasma"),
                display_name: "Test".into(),
                category: "Procedural".into(),
                osc_prefix: "test".into(),
                legacy_discriminant: None,
                available: true,
                is_line_based: false,
                params: vec![ParamSpecDef {
                    id: "pattern".into(),
                    name: "Pattern".into(),
                    min: 0.0,
                    max: 7.0,
                    default_value: 0.0,
                    whole_numbers: true,
                    is_toggle: false,
                    is_trigger: false,
                    value_labels: vec![],
                    format_string: None,
                    osc_suffix: String::new(),
                    curve: Default::default(),
                    invert: false,
                    is_angle: false,
                    is_trigger_gate: false,
                    wraps: false,
                    section: None,
                }],
                bindings: vec![BindingDef {
                    id: "pattern".into(),
                    label: "Pattern".into(),
                    default_value: 0.0,
                    target: BindingTarget::Node {
                        node_id: manifold_core::NodeId::new("gen"),
                        param: "pattern".into(),
                    },
                    convert: ParamConvert::EnumRound,
                    user_added: false,
                    scale: 1.0,
                    offset: 0.0,
                }],
                skip_mode: Default::default(),
                param_aliases: vec![],
                value_aliases: vec![],
                string_params: vec![],
                string_bindings: vec![],
            }),
            nodes: vec![EffectGraphNode {
                id: 0,
                node_id: manifold_core::NodeId::new("gen"),
                type_id: "node.plasma_pattern_2d".to_string(),
                handle: Some("gen".to_string()),
                params: BTreeMap::new(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            }],
            wires: vec![],
        };

        // Use a generator target so we don't drag in the effect-side
        // mirror. Same exposure semantics apply for both.
        let (mut project, lid) = project_with_one_generator_layer();

        // Pre-populate the layer's override with the preset def
        // (simulates "graph has been touched once already" — needed
        // because `with_target_graph_mut` would otherwise clone the
        // catalog_default, and we want a deterministic starting state).
        project
            .timeline
            .find_layer_by_id_mut(&lid)
            .unwrap()
            .1
            .gen_params_or_init().graph = Some(preset_def_with_pattern_binding());

        // UNCHECK Pattern.
        let mut cmd = ToggleNodeParamExposeCommand::new(
            GraphTarget::Generator(lid.clone()),
            manifold_core::NodeId::new("gen"),
            0,
            "gen".to_string(),
            "pattern".to_string(),
            false,
            preset_def_with_pattern_binding(),
            "Pattern".to_string(),
            0.0,
            7.0,
            0.0,
            ParamConvert::EnumRound,
            false,
            Vec::new(),
        );
        cmd.execute(&mut project);

        // The def must NOT contain "pattern" in exposed_params for
        // the "gen" node.
        let (_, layer) = project.timeline.find_layer_by_id(&lid).unwrap();
        let def = layer.generator_graph().unwrap();
        let node = def
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("gen"))
            .unwrap();
        assert!(
            !node.exposed_params.contains("pattern"),
            "uncheck removes pattern from exposed_params"
        );

        // Now persist + reload: serde JSON round-trip simulating a
        // save/reload cycle.
        let json = serde_json::to_string(def).unwrap();
        let reloaded: EffectGraphDef = serde_json::from_str(&json).unwrap();
        // The reloaded def must STILL not have pattern exposed. The
        // semantics: an empty exposed_params set on a node coexists
        // with other nodes having non-empty sets, so the implicit
        // backfill at `into_graph` time must respect explicit state.
        let reloaded_node = reloaded
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("gen"))
            .unwrap();
        assert!(
            !reloaded_node.exposed_params.contains("pattern"),
            "uncheck survives serde round-trip"
        );
    }

    #[test]
    fn toggle_node_param_expose_against_effect_flips_both_graph_and_user_binding() {
        // Project with one master effect using the catalog default.
        let mut project = Project::default();
        let effect_id = EffectId::new("test-mirror-instance");
        let mut fx = PresetInstance::new(PresetTypeId::new("test.mirror"));
        fx.id = effect_id.clone();
        project.settings.master_effects.push(fx);

        let mut cmd = ToggleNodeParamExposeCommand::new(
            GraphTarget::Effect(effect_id.clone()),
            manifold_core::NodeId::new("uv_transform"),
            1,
            "uv_transform".to_string(),
            "rotation".to_string(),
            true,
            mirror_catalog_default(),
            "Rotation".to_string(),
            -180.0,
            180.0,
            0.0,
            manifold_core::effects::ParamConvert::Float,
            false,
            Vec::new(),
        );

        cmd.execute(&mut project);

        let fx = project.find_effect_by_id(&effect_id).unwrap();
        // Graph side: exposed_params set carries the param.
        let def = fx.graph.as_ref().expect("graph lifted on first edit");
        let node = def
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("uv_transform"))
            .unwrap();
        assert!(node.exposed_params.contains("rotation"));
        // Effect-side mirror: a user-added binding was appended to the
        // graph metadata because the catalog default has no preset
        // bindings for this param.
        let ub = fx.user_param_bindings();
        assert_eq!(ub.len(), 1);
        assert_eq!(ub[0].node_id, "uv_transform");
        assert_eq!(ub[0].inner_param, "rotation");

        // Undo reverses both sides.
        cmd.undo(&mut project);
        let fx = project.find_effect_by_id(&effect_id).unwrap();
        let def = fx.graph.as_ref().unwrap();
        let node = def
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("uv_transform"))
            .unwrap();
        assert!(!node.exposed_params.contains("rotation"));
        assert_eq!(fx.user_param_bindings().len(), 0);
    }

    /// `PARAM_TWO_WAY_BINDING_DESIGN.md` D9: unmapping a user-added binding
    /// freezes the card's current effective value into the def slot the
    /// binding stops governing, instead of leaving whatever stale value sat
    /// there — so the render never visually snaps on unmap.
    #[test]
    fn unexpose_user_binding_freezes_effective_value_into_def_slot() {
        let mut project = Project::default();
        let effect_id = EffectId::new("test-mirror-instance-freeze");
        let mut fx = PresetInstance::new(PresetTypeId::new("test.mirror"));
        fx.id = effect_id.clone();
        project.settings.master_effects.push(fx);

        // Expose rotation (appends a user binding).
        let mut expose_cmd = ToggleNodeParamExposeCommand::new(
            GraphTarget::Effect(effect_id.clone()),
            manifold_core::NodeId::new("uv_transform"),
            1,
            "uv_transform".to_string(),
            "rotation".to_string(),
            true,
            mirror_catalog_default(),
            "Rotation".to_string(),
            -180.0,
            180.0,
            0.0,
            manifold_core::effects::ParamConvert::Float,
            false,
            Vec::new(),
        );
        expose_cmd.execute(&mut project);

        // Move the card away from default, as a performer would — through
        // the same command the card's own slider drag commits via
        // (`ChangeGraphParamCommand`, `commands/effects.rs`), not a raw
        // manifest poke.
        let binding_id = project
            .find_effect_by_id(&effect_id)
            .unwrap()
            .user_param_bindings()[0]
            .id
            .clone();
        let mut set_cmd = crate::commands::effects::ChangeGraphParamCommand::new(
            GraphTarget::Effect(effect_id.clone()),
            binding_id,
            0.0,
            77.0,
        );
        set_cmd.execute(&mut project);

        // Unexpose — this removes the user binding and must freeze 77.0
        // into the def's `rotation` slot before the binding goes away.
        let mut unexpose_cmd = ToggleNodeParamExposeCommand::new(
            GraphTarget::Effect(effect_id.clone()),
            manifold_core::NodeId::new("uv_transform"),
            1,
            "uv_transform".to_string(),
            "rotation".to_string(),
            false,
            mirror_catalog_default(),
            "Rotation".to_string(),
            -180.0,
            180.0,
            0.0,
            manifold_core::effects::ParamConvert::Float,
            false,
            Vec::new(),
        );
        unexpose_cmd.execute(&mut project);

        let fx = project.find_effect_by_id(&effect_id).unwrap();
        assert_eq!(fx.user_param_bindings().len(), 0, "binding removed");
        let def = fx.graph.as_ref().unwrap();
        let node = def
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("uv_transform"))
            .unwrap();
        match node.params.get("rotation") {
            Some(SerializedParamValue::Float { value }) => {
                assert!((value - 77.0).abs() < 1e-6, "expected frozen 77.0, got {value}");
            }
            other => panic!("expected a frozen Float value, got {other:?}"),
        }
    }
}
