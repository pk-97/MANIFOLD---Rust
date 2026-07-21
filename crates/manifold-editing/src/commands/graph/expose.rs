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

