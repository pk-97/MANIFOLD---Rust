//! Graph-editor bridge: the watched-graph reads, param-mapping preview/commit,
//! preset-save/rename prompts, node copy, the bound/unbound node-param drag
//! sessions, and the graph-editor window present. Moved verbatim from
//! app_render.rs (UI_FUNNEL_DECOMPOSITION P-F1, pure move).

use crate::app::Application;
use crate::content_command::ContentCommand;
use manifold_editing::commands::effects::BindingMappingEdit;

/// Build the reshape-edit command for the watched graph target — one
/// [`manifold_editing::commands::effects::EditParamMappingCommand`] for
/// both effects and generators. The reshape lives in the preset's
/// authoring surface (`ParamSpecDef` + `BindingDef`); the command edits the
/// instance's per-instance graph override, materializing it from `seed_def`
/// (the catalog graph, resolved renderer-side) when the instance is still on
/// the catalog default. Addresses the param by stable id.
pub(crate) fn build_mapping_command(
    target: &manifold_core::GraphTarget,
    param_id: &str,
    edit: manifold_editing::commands::effects::BindingMappingEdit,
    seed_def: Option<manifold_core::effect_graph_def::EffectGraphDef>,
) -> Box<dyn manifold_editing::command::Command + Send> {
    Box::new(
        manifold_editing::commands::effects::EditParamMappingCommand::new(
            target.clone(),
            param_id.to_string(),
            edit,
            seed_def,
        ),
    )
}

/// `Application::seed_def_for` minus the `&self` — the catalog/bundled
/// default def a mapping edit seeds from when the instance hasn't diverged,
/// resolvable from any `&Project` (the `ResolvedScrub::Mapping{Range,Affine}`
/// mapping-drag restore in `ui_bridge::scrub` uses it, same as `preview_mapping`
/// does here).
pub(crate) fn seed_def_for_project(
    project: &manifold_core::project::Project,
    target: &manifold_core::GraphTarget,
) -> Option<manifold_core::effect_graph_def::EffectGraphDef> {
    let owner = project.graph_target_owner(target)?;
    if owner.graph.is_some() { return None; }
    crate::graph_target::owner_default(project, target)
}

/// Drag-commit variant: the command carries the EXPLICIT pre-drag reverse
/// (captured at drag start) so undo restores the true pre-drag values, not
/// the preview-mutated ones — mirroring `ChangeEffectParamCommand`'s
/// explicit `old_value`.
fn build_mapping_command_with_reverse(
    target: &manifold_core::GraphTarget,
    param_id: &str,
    new: manifold_editing::commands::effects::BindingMappingEdit,
    reverse: manifold_editing::commands::effects::BindingMappingEdit,
    seed_def: Option<manifold_core::effect_graph_def::EffectGraphDef>,
) -> Box<dyn manifold_editing::command::Command + Send> {
    Box::new(
        manifold_editing::commands::effects::EditParamMappingCommand::new_with_reverse(
            target.clone(),
            param_id.to_string(),
            new,
            reverse,
            seed_def,
        ),
    )
}

/// Immutable descend into a graph def at a scope path of group ids — the def
/// analog of the snapshot's `resolve_level`. `None` if the path doesn't resolve
/// (a group id is missing or isn't a group). Empty scope is the document root.
fn descend_def_level<'a>(
    def: &'a manifold_core::effect_graph_def::EffectGraphDef,
    scope: &[u32],
) -> Option<(
    &'a [manifold_core::effect_graph_def::EffectGraphNode],
    &'a [manifold_core::effect_graph_def::EffectGraphWire],
)> {
    let mut nodes = def.nodes.as_slice();
    let mut wires = def.wires.as_slice();
    for &gid in scope {
        let group = nodes.iter().find(|n| n.id == gid)?.group.as_deref()?;
        nodes = &group.nodes;
        wires = &group.wires;
    }
    Some((nodes, wires))
}

/// Read the live manifest spec and the graph's binding affine transform.
/// Graph metadata's param specs are a serialization shadow, not live UI state.
fn full_reshape_from_instance(
    instance: &manifold_core::effects::PresetInstance,
    def: &manifold_core::effect_graph_def::EffectGraphDef,
    param_id: &str,
) -> Option<(
    String,
    f32,
    f32,
    bool,
    manifold_core::macro_bank::MacroCurve,
    f32,
    f32,
)> {
    let meta = def.preset_metadata.as_ref()?;
    let param = instance.params.get(param_id)?;
    let spec = &param.spec;
    let (span_min, span_max) = param.slider_span();
    let (scale, offset) = meta
        .bindings
        .iter()
        .find(|b| b.id == param_id)
        .map(|b| (b.scale, b.offset))
        .unwrap_or((1.0, 0.0));
    Some((
        spec.name.clone(),
        span_min,
        span_max,
        spec.invert,
        spec.curve,
        scale,
        offset,
    ))
}

/// Read-only mirror of `manifold_editing::commands::graph::descend_level`'s
/// node-list walk — the editing crate's version is `&mut` and private, and
/// this dispatch-layer lookup (`binding_for_node_param`) only ever reads.
/// `None` if a hop in `scope_path` doesn't resolve to a group node.
fn descend_level_ref<'a>(
    nodes: &'a [manifold_core::effect_graph_def::EffectGraphNode],
    scope_path: &[u32],
) -> Option<&'a [manifold_core::effect_graph_def::EffectGraphNode]> {
    let mut cur = nodes;
    for &gid in scope_path {
        let group = cur.iter().find(|n| n.id == gid)?.group.as_ref()?;
        cur = &group.nodes;
    }
    Some(cur)
}

/// Resolve the card binding (if any) governing `(node_id, param_name)` at
/// `scope_path` within `def` — BUG-158 write-back (D1/D2,
/// `docs/PARAM_TWO_WAY_BINDING_DESIGN.md`). Descends to the node's level,
/// reads its stable `NodeId`, then looks that up against
/// `preset_metadata.bindings` (the single unified list post
/// `PRESET_UNIFICATION_PLAN.md` — bundled and user-added bindings both live
/// here). Mirrors [`full_reshape_from_instance`]'s reshape lookup, keyed the
/// other way (by node target instead of outer id). `None` when the node has
/// no stable id (a bundled node that's never been targeted), the level
/// doesn't resolve, or no binding targets this param.
fn binding_for_node_param(
    params: &manifold_core::params::ParamManifest,
    def: &manifold_core::effect_graph_def::EffectGraphDef,
    scope_path: &[u32],
    node_id: u32,
    param_name: &str,
) -> Option<(
    String,
    f32,
    f32,
    bool,
    manifold_core::macro_bank::MacroCurve,
    f32,
    f32,
)> {
    use manifold_core::effect_graph_def::BindingTarget;
    let meta = def.preset_metadata.as_ref()?;
    let nodes = descend_level_ref(&def.nodes, scope_path)?;
    let node = nodes.iter().find(|n| n.id == node_id)?;
    if node.node_id.is_empty() {
        return None;
    }
    let binding = meta.bindings.iter().find(|b| match &b.target {
        BindingTarget::Node { node_id: nid, param } => {
            *nid == node.node_id && param == param_name
        }
        BindingTarget::Composite { .. } | BindingTarget::SceneModifier { .. } => false,
    })?;
    let param = params.get(&binding.id)?;
    let spec = &param.spec;
    let (span_min, span_max) = param.slider_span();
    Some((
        binding.id.clone(),
        span_min,
        span_max,
        spec.invert,
        spec.curve,
        binding.scale,
        binding.offset,
    ))
}

/// `true` when `(node_id, param_name)` at `scope_path` has an incoming wire
/// — the P1 enforcement backstop for the "a bound graph param slot is never
/// written by the node-face path" invariant. D5/D6 (wire beats binding) make
/// the UI prevent a scrub from ever starting on a wired row; this is the
/// dispatch-layer's own guard should that prevention somehow not fire.
fn node_param_is_wired(
    def: &manifold_core::effect_graph_def::EffectGraphDef,
    scope_path: &[u32],
    node_id: u32,
    param_name: &str,
) -> bool {
    let mut wires = &def.wires;
    let mut level_nodes = &def.nodes;
    for &gid in scope_path {
        let Some(group) = level_nodes.iter().find(|n| n.id == gid).and_then(|n| n.group.as_ref())
        else {
            return false;
        };
        wires = &group.wires;
        level_nodes = &group.nodes;
    }
    wires
        .iter()
        .any(|w| w.to_node == node_id && w.to_port == param_name)
}

/// Read `(node_id, param_name)`'s current value at `scope_path` within
/// `def` — the BUG-282 pre-drag-baseline read. `None` distinguishes "no
/// stored override" (falls back to the primitive's own default at apply
/// time) from an unresolvable scope/node, both of which the caller treats
/// the same way (`with_previous(None)`).
pub(crate) fn node_param_value(
    def: &manifold_core::effect_graph_def::EffectGraphDef,
    scope_path: &[u32],
    node_id: u32,
    param_name: &str,
) -> Option<manifold_core::effect_graph_def::SerializedParamValue> {
    let nodes = descend_level_ref(&def.nodes, scope_path)?;
    let node = nodes.iter().find(|n| n.id == node_id)?;
    node.params.get(param_name).cloned()
}

/// Extract the scalar f32 value from a graph-def `SerializedParamValue`.
/// `None` for the non-scalar kinds (`Vec*`, `Color`, `Table`, `String`) —
/// card bindings are scalar-only (`BindingDef::default_value: f32`), so a
/// non-scalar edit can never be a bound-param reroute candidate.
pub(crate) fn serialized_value_as_f32(
    v: &manifold_core::effect_graph_def::SerializedParamValue,
) -> Option<f32> {
    use manifold_core::effect_graph_def::SerializedParamValue as V;
    match v {
        V::Float { value } => Some(*value),
        V::Int { value } => Some(*value as f32),
        V::Bool { value } => Some(if *value { 1.0 } else { 0.0 }),
        V::Enum { value } => Some(*value as f32),
        V::Vec2 { .. } | V::Vec3 { .. } | V::Vec4 { .. } | V::Color { .. } | V::Table { .. } | V::String { .. } => {
            None
        }
    }
}

/// Convert a renderer-side [`manifold_nodes::node_graph::NodeSnapshot`]
/// into the UI-facing [`manifold_ui::panels::graph_editor::GraphEditorNodeView`]
/// that the right-sidebar panel consumes.
///
/// Returns `None` when:
/// - no graph snapshot is available, or
/// - the canvas's selected node is not in the snapshot.
///
/// Intentionally does NOT gate on an effect target — generator graphs
/// have no effect identity, but the snapshot carries everything the
/// per-node param view needs (handle, title, params + ranges). Gating
/// on an effect-only target would silently empty the right column for
/// every generator graph the user opens.
/// Recursively find a snapshot node by stable [`NodeId`], descending into
/// groups. Resolves a previewed node's title + type_id for the value inspector.
/// What the editor's node inspector title and description were built from.
/// Holding the snapshot keeps its identity unique while compared.
pub(crate) struct NodeInspectorKey {
    node: manifold_core::NodeId,
    diagnostic: Option<&'static str>,
    snapshot: Option<std::sync::Arc<manifold_node_engine::snapshot::GraphSnapshot>>,
}

impl NodeInspectorKey {
    fn of(
        info: &crate::content_state::NodePreviewInfo,
        snapshot: Option<&std::sync::Arc<manifold_node_engine::snapshot::GraphSnapshot>>,
    ) -> Self {
        Self { node: info.node_id.clone(), diagnostic: info.diagnostic, snapshot: snapshot.cloned() }
    }

    fn matches(
        &self,
        info: &crate::content_state::NodePreviewInfo,
        snapshot: Option<&std::sync::Arc<manifold_node_engine::snapshot::GraphSnapshot>>,
    ) -> bool {
        self.node == info.node_id
            && self.diagnostic == info.diagnostic
            && match (&self.snapshot, snapshot) {
                (Some(a), Some(b)) => std::sync::Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            }
    }
}

fn find_snapshot_node<'a>(
    nodes: &'a [manifold_node_engine::snapshot::NodeSnapshot],
    id: &manifold_core::NodeId,
) -> Option<&'a manifold_node_engine::snapshot::NodeSnapshot> {
    for n in nodes {
        if &n.node_id == id {
            return Some(n);
        }
        if let Some(g) = n.group.as_ref()
            && let Some(found) = find_snapshot_node(&g.nodes, id)
        {
            return Some(found);
        }
    }
    None
}

/// Resolve the selected canvas node — which may be a *boundary* node that has
/// no runtime instance of its own — to a concrete preview-target [`NodeId`] the
/// content thread can capture. Walks the hierarchical snapshot at the canvas
/// scope:
///
/// - A plain node previews itself.
/// - A **Group Output** boundary previews the enclosing group's *container*
///   node_id, so the content side resolves it through the existing
///   `group_preview_map` (producer + interface port name for encoding).
/// - A **Group Input** boundary previews the external node feeding that group's
///   primary input, one scope level up.
/// - A `final_output` sink previews its input producer.
/// - A sub-group producer is previewed by its container node_id (also via
///   `group_preview_map`); a `group_input` producer recurses to the parent feed.
///
/// Pure over the snapshot, so it's unit-tested below.
fn resolve_preview_target(
    snap: &manifold_ui::graph_view::GraphSnapshot,
    scope: &[u32],
    selected: u32,
) -> Option<manifold_core::NodeId> {
    let (nodes, wires) =
        crate::graph_canvas::resolve_level(snap, scope).unwrap_or((&snap.nodes, &snap.wires));
    let node = nodes.iter().find(|n| n.id == selected)?;
    resolve_boundary_node(node, nodes, wires, snap, scope)
}

fn resolve_boundary_node(
    node: &manifold_ui::graph_view::NodeSnapshot,
    nodes: &[manifold_ui::graph_view::NodeSnapshot],
    wires: &[manifold_ui::graph_view::WireSnapshot],
    snap: &manifold_ui::graph_view::GraphSnapshot,
    scope: &[u32],
) -> Option<manifold_core::NodeId> {
    use manifold_core::effect_graph_def::{GROUP_INPUT_TYPE_ID, GROUP_OUTPUT_TYPE_ID};
    match node.type_id.as_str() {
        GROUP_OUTPUT_TYPE_ID => {
            // The enclosing group's output. Send the group container's node_id;
            // the content side maps it to the producer (+ port name) itself.
            let (group_u32, parent) = scope.split_last()?;
            let (p_nodes, _) =
                crate::graph_canvas::resolve_level(snap, parent).unwrap_or((&snap.nodes, &snap.wires));
            non_empty_node_id(&p_nodes.iter().find(|n| n.id == *group_u32)?.node_id)
        }
        GROUP_INPUT_TYPE_ID => {
            // Data entering the enclosing group: its external feeder, one level up.
            // The boundary's outputs are the group's input ports.
            let port = primary_texture_port(&node.outputs)?;
            let (group_u32, parent) = scope.split_last()?;
            let (p_nodes, p_wires) =
                crate::graph_canvas::resolve_level(snap, parent).unwrap_or((&snap.nodes, &snap.wires));
            let producer = producer_into(*group_u32, port, p_nodes, p_wires)?;
            resolve_producer(producer, p_nodes, p_wires, snap, parent)
        }
        "system.final_output" => {
            let port = primary_texture_port(&node.inputs)?;
            let producer = producer_into(node.id, port, nodes, wires)?;
            resolve_producer(producer, nodes, wires, snap, scope)
        }
        // Plain node, Source, generator_input: preview the node itself.
        _ => non_empty_node_id(&node.node_id),
    }
}

/// Concrete preview target for a producer node: a sub-group resolves by its
/// container id (content `group_preview_map`); a `group_input` producer recurses
/// to the parent feed; anything else previews itself.
fn resolve_producer(
    producer: &manifold_ui::graph_view::NodeSnapshot,
    nodes: &[manifold_ui::graph_view::NodeSnapshot],
    wires: &[manifold_ui::graph_view::WireSnapshot],
    snap: &manifold_ui::graph_view::GraphSnapshot,
    scope: &[u32],
) -> Option<manifold_core::NodeId> {
    use manifold_core::effect_graph_def::GROUP_INPUT_TYPE_ID;
    if producer.group.is_some() {
        return non_empty_node_id(&producer.node_id);
    }
    if producer.type_id == GROUP_INPUT_TYPE_ID {
        return resolve_boundary_node(producer, nodes, wires, snap, scope);
    }
    non_empty_node_id(&producer.node_id)
}

/// Resolve a selected node and retain the authored group scope alongside its
/// stable id. This follows the numeric canvas path at every boundary hop, so
/// identical node ids in sibling groups cannot be confused by a global search.
fn resolve_preview_target_with_scope(
    snap: &manifold_ui::graph_view::GraphSnapshot,
    scope: &[u32],
    selected: u32,
) -> Option<(manifold_core::NodeId, Vec<manifold_core::NodeId>)> {
    use manifold_core::effect_graph_def::{GROUP_INPUT_TYPE_ID, GROUP_OUTPUT_TYPE_ID};
    let (nodes, wires) = crate::graph_canvas::resolve_level(snap, scope)?;
    let stable_scope = stable_scope_path(snap, scope)?;
    let node = nodes.iter().find(|node| node.id == selected)?;
    match node.type_id.as_str() {
        GROUP_OUTPUT_TYPE_ID => {
            let (group_id, parent_scope) = scope.split_last()?;
            let (parent_nodes, _) = crate::graph_canvas::resolve_level(snap, parent_scope)?;
            let group = parent_nodes.iter().find(|node| node.id == *group_id)?;
            Some((non_empty_node_id(&group.node_id)?, stable_scope_path(snap, parent_scope)?))
        }
        GROUP_INPUT_TYPE_ID => {
            let port = primary_texture_port(&node.outputs)?;
            let (group_id, parent_scope) = scope.split_last()?;
            let (parent_nodes, parent_wires) = crate::graph_canvas::resolve_level(snap, parent_scope)?;
            let producer = producer_into(*group_id, port, parent_nodes, parent_wires)?;
            resolve_preview_target_with_scope(snap, parent_scope, producer.id)
        }
        "system.final_output" => {
            let port = primary_texture_port(&node.inputs)?;
            let producer = producer_into(node.id, port, nodes, wires)?;
            resolve_preview_target_with_scope(snap, scope, producer.id)
        }
        _ => Some((non_empty_node_id(&node.node_id)?, stable_scope)),
    }
}

/// The node feeding `(to_node, to_port)`, or any wire into `to_node` as a
/// fallback when the exact port name doesn't match.
fn producer_into<'a>(
    to_node: u32,
    to_port: &str,
    nodes: &'a [manifold_ui::graph_view::NodeSnapshot],
    wires: &[manifold_ui::graph_view::WireSnapshot],
) -> Option<&'a manifold_ui::graph_view::NodeSnapshot> {
    let w = wires
        .iter()
        .find(|w| w.to_node == to_node && w.to_port == to_port)
        .or_else(|| wires.iter().find(|w| w.to_node == to_node))?;
    nodes.iter().find(|n| n.id == w.from_node)
}

/// First `Texture2D`(-typed) port name, else the first port of any type.
fn primary_texture_port(ports: &[manifold_ui::graph_view::PortSnapshot]) -> Option<&str> {
    use manifold_ui::graph_view::PortKindSnapshot;
    ports
        .iter()
        .find(|p| {
            matches!(
                p.kind,
                PortKindSnapshot::Texture2D | PortKindSnapshot::Texture2DTyped { .. }
            )
        })
        .or_else(|| ports.first())
        .map(|p| p.name.as_str())
}

fn non_empty_node_id(id: &manifold_core::NodeId) -> Option<manifold_core::NodeId> {
    if id.as_str().is_empty() {
        None
    } else {
        Some(id.clone())
    }
}

/// Translate the canvas's numeric group path to the authored stable group
/// identities used by modifier preview routes. The numeric ids are local to a
/// snapshot level and must never cross the editor/runtime boundary.
fn stable_scope_path(
    snap: &manifold_ui::graph_view::GraphSnapshot,
    scope: &[u32],
) -> Option<Vec<manifold_core::NodeId>> {
    let mut level = snap.nodes.as_slice();
    let mut out = Vec::with_capacity(scope.len());
    for &group_id in scope {
        let group = level.iter().find(|node| node.id == group_id)?;
        if group.node_id.is_empty() {
            return None;
        }
        out.push(group.node_id.clone());
        level = &group.group.as_ref()?.nodes;
    }
    Some(out)
}

fn modifier_preview_selection(
    snap: &manifold_ui::graph_view::GraphSnapshot,
    canvas_scope: &[u32],
    selected: Option<u32>,
) -> (Option<manifold_core::NodeId>, Vec<manifold_core::NodeId>) {
    let fallback = stable_scope_path(snap, canvas_scope).unwrap_or_default();
    let Some(selected) = selected else { return (None, fallback); };
    resolve_preview_target_with_scope(snap, canvas_scope, selected)
        .map(|(node, scope)| (Some(node), scope))
        .unwrap_or((None, fallback))
}

/// Resolve an on-canvas param row `(node_id, inner_param)` to the
/// matching card binding on the watched effect or generator, returning the
/// data the mapping popover needs to open. `None` when there's no active
/// snapshot/target, the node has no stable handle, or the inner param
/// has no live exposed binding. Stock and user-added bindings use the same path.
///
/// Returned tuple: `(binding_id, label, min, max, invert, curve, scale, offset, range, is_angle, section)`.
/// `range` is the binding's declared inner-param bounds, used to span the
/// popover's trim track.
///
/// Free function (not a method) so the editor-window mouse handler can
/// call it while the `&mut GraphCanvas` borrow is live: it takes the
/// disjoint `self` fields (snapshot, target, project) by reference rather
/// than borrowing all of `self`.
#[allow(clippy::type_complexity)]
pub(crate) fn resolve_canvas_binding(
    snapshot: Option<&manifold_node_engine::snapshot::GraphSnapshot>,
    target: Option<&manifold_core::GraphTarget>,
    project: &manifold_core::project::Project,
    node_id: u32,
    inner_param: &str,
) -> Option<(
    String,
    String,
    f32,
    f32,
    bool,
    manifold_core::macro_bank::MacroCurve,
    f32,
    f32,
    Option<(f32, f32)>,
    bool,
    Option<String>,
)> {
    let snap = snapshot?;
    // Canvas runtime id → the node's stable NodeId (anonymous boundary
    // nodes have an empty id and can't carry bindings).
    let node = snap.nodes.iter().find(|n| n.id == node_id)?;
    if node.node_id.is_empty() {
        return None;
    }
    // Declared inner-param range, for the trim track span.
    let range = node
        .parameters
        .iter()
        .find(|p| p.name == inner_param)
        .and_then(|p| p.range);
    let is_angle = node
        .parameters
        .iter()
        .find(|p| p.name == inner_param)
        .is_some_and(|p| p.kind == manifold_node_engine::snapshot::ParamSnapshotKind::Angle);
    if matches!(target?, manifold_core::GraphTarget::SceneModifier { .. }) {
        let target = target.expect("scene modifier target was checked above");
        let owner = project.graph_target_owner(target)?;
        let local = crate::graph_target::resolve(project, target)?;
        let local_meta = local.preset_metadata.as_ref()?;
        let local_binding = local_meta.bindings.iter().find(|binding| {
            matches!(&binding.target, manifold_core::effect_graph_def::BindingTarget::Node { node_id, param }
                if *node_id == node.node_id && param == inner_param)
        })?;
        let outer = project
            .graph_target_owner(target)?
            .graph
            .as_ref()
            .and_then(|graph| graph.preset_metadata.as_ref())?
            .bindings
            .iter()
            .find(|binding| matches!(&binding.target,
                manifold_core::effect_graph_def::BindingTarget::SceneModifier { modifier_id, param_id }
                    if target_modifier_id(target) == Some(modifier_id) && param_id == &local_binding.id))?;
        return crate::graph_target::modifier_host_binding(
            project,
            target,
            &outer.id,
            |outer| {
                let param = owner.params.get(&outer.id)?;
                let (span_min, span_max) = param.slider_span();
                let spec = param.spec.clone();
                Some((
                    outer.id.clone(), outer.label.clone(), span_min, span_max,
                    spec.invert, spec.curve, outer.scale, outer.offset,
                    range, is_angle, spec.section,
                ))
            },
        ).flatten();
    }
    let instance = project.preset_instance(target?)?;
    let view;
    let def = if let Some(def) = instance.graph.as_ref() {
        def
    } else {
        view = manifold_node_engine::load::loaded_preset_view::loaded_preset_view_by_id(instance.effect_type())?;
        view.canonical_def.as_ref()
    };
    let binding = def.preset_metadata.as_ref()?.bindings.iter().find(|b| {
        matches!(&b.target, manifold_core::effect_graph_def::BindingTarget::Node { node_id, param }
            if *node_id == node.node_id && param == inner_param)
    })?;
    let (label, min, max, invert, curve, scale, offset) =
        full_reshape_from_instance(instance, def, &binding.id)?;
    Some((binding.id.clone(), label, min, max, invert, curve, scale, offset,
        range, is_angle, instance.params.get(&binding.id)?.spec.section.clone()))
}

fn target_modifier_id(target: &manifold_core::GraphTarget) -> Option<&manifold_core::NodeId> {
    match target {
        manifold_core::GraphTarget::SceneModifier { modifier_id, .. } => Some(modifier_id),
        _ => None,
    }
}

fn modifier_binding_for_node_param(
    project: &manifold_core::project::Project,
    target: &manifold_core::GraphTarget,
    scope_path: &[u32],
    node_id: u32,
    param_name: &str,
) -> Option<(
    String,
    f32,
    f32,
    bool,
    manifold_core::macro_bank::MacroCurve,
    f32,
    f32,
)> {
    let modifier_id = target_modifier_id(target)?;
    let owner = project.graph_target_owner(target)?;
    let local = crate::graph_target::resolve(project, target)?;
    let nodes = descend_level_ref(&local.nodes, scope_path)?;
    let node = nodes.iter().find(|node| node.id == node_id)?;
    if node.node_id.is_empty() {
        return None;
    }
    let local_binding = local
        .preset_metadata
        .as_ref()?
        .bindings
        .iter()
        .find(|binding| matches!(&binding.target,
            manifold_core::effect_graph_def::BindingTarget::Node { node_id, param }
                if *node_id == node.node_id && param == param_name))?;
    let owner_binding = owner
        .graph
        .as_ref()
        .and_then(|graph| graph.preset_metadata.as_ref())?
        .bindings
        .iter()
        .find(|binding| matches!(&binding.target,
            manifold_core::effect_graph_def::BindingTarget::SceneModifier { modifier_id: id, param_id }
                if id == modifier_id && param_id == &local_binding.id))?;
    let param = owner.params.get(&owner_binding.id)?;
    let spec = &param.spec;
    let (span_min, span_max) = param.slider_span();
    Some((
        owner_binding.id.clone(), span_min, span_max, spec.invert, spec.curve,
        owner_binding.scale * local_binding.scale,
        owner_binding.offset * local_binding.scale + local_binding.offset,
    ))
}

/// Points the graph editor at `target`: both mapping popovers close when the
/// target changes (closing cancels an unfinished gesture; events already
/// queued keep their captured owner), the catalog default the edit commands
/// seed from resolves, and the content side starts snapshotting it. False,
/// and nothing changes, when the target has no host and so never resolves.
pub(crate) fn watch_editor_graph(
    project: &manifold_core::project::Project,
    target: &manifold_core::GraphTarget,
    watched: &mut Option<manifold_core::GraphTarget>,
    catalog_default: &mut Option<manifold_core::effect_graph_def::EffectGraphDef>,
    mapping: &mut crate::mapping_popover::MappingPopover,
    canvas: Option<&mut crate::graph_canvas::GraphCanvas>,
    content_tx: Option<&crossbeam_channel::Sender<ContentCommand>>,
) -> bool {
    if target.host_target().is_none() {
        return false;
    }
    if watched.as_ref() != Some(target) {
        mapping.close();
        if let Some(canvas) = canvas {
            canvas.close_mapping_popover();
        }
    }
    *catalog_default = crate::graph_target::catalog_default(project, target);
    if let Some(tx) = content_tx {
        ContentCommand::send(tx, ContentCommand::WatchGraphTarget(Some(target.clone())));
    }
    *watched = Some(target.clone());
    true
}

/// An effect card's stable id from its row index in an inspector tab, keyed
/// by instance id and tab-aware (Master, Layer or Group, Clip).
pub(crate) fn effect_card_id(
    tab: manifold_ui::InspectorTab,
    project: &manifold_core::project::Project,
    active_layer: Option<&manifold_core::LayerId>,
    selection: &crate::app::SelectionState,
    ei: usize,
) -> Option<manifold_core::EffectId> {
    match tab {
        manifold_ui::InspectorTab::Master => project.settings.master_effects.get(ei).map(|e| e.id.clone()),
        manifold_ui::InspectorTab::Layer | manifold_ui::InspectorTab::Group | manifold_ui::InspectorTab::Scene => active_layer
            .and_then(|id| project.timeline.find_layer_by_id(id))
            .and_then(|(_, l)| l.effects.as_ref())
            .and_then(|effects| effects.get(ei))
            .map(|e| e.id.clone()),
        manifold_ui::InspectorTab::Clip => selection
            .primary_selected_clip_id()
            .as_ref()
            .and_then(|cid| project.timeline.layers.iter().flat_map(|l| l.clips.iter()).find(|c| c.id == **cid))
            .and_then(|c| c.effects.get(ei))
            .map(|e| e.id.clone()),
    }
}

/// Mapping controls resolve their captured owner, independently of canvas selection.
pub(crate) fn target_full_reshape(
    project: &manifold_core::project::Project,
    target: &manifold_core::GraphTarget,
    param_id: &str,
) -> Option<(String, f32, f32, bool, manifold_core::macro_bank::MacroCurve, f32, f32)> {
    if matches!(target, manifold_core::GraphTarget::SceneModifier { .. }) {
        let owner = project.graph_target_owner(target)?;
        let binding = crate::graph_target::modifier_host_binding(
            project,
            target,
            param_id,
            |binding| (binding.scale, binding.offset),
        )?;
        let param = owner.params.get(param_id)?;
        let spec = &param.spec;
        let (span_min, span_max) = param.slider_span();
        return Some((
            spec.name.clone(), span_min, span_max, spec.invert, spec.curve,
            binding.0, binding.1,
        ));
    }
    let instance = project.preset_instance(target)?;
    if let Some(def) = instance.graph.as_ref() {
        return full_reshape_from_instance(instance, def, param_id);
    }
    let view = manifold_node_engine::load::loaded_preset_view::loaded_preset_view_by_id(instance.effect_type())?;
    full_reshape_from_instance(instance, view.canonical_def.as_ref(), param_id)
}

/// Mapping controls on a modifier are read through the local snapshot but
/// always write the owning generator's public manifest. Validate the local
/// binding first so stale or preparation-only rows cannot materialize an
/// unrelated host graph.
fn mapping_write_target(
    project: &manifold_core::project::Project,
    target: &manifold_core::GraphTarget,
    param_id: &str,
) -> Option<manifold_core::GraphTarget> {
    if matches!(target, manifold_core::GraphTarget::SceneModifier { .. }) {
        target_full_reshape(project, target, param_id)?;
        return Some(target.host_target()?.clone());
    }
    Some(target.clone())
}

/// A card's mapping drawer, resolved from the emitting card and ready to open.
pub(crate) struct CardMapping {
    /// The graph the drawer edits; the caller watches it first.
    pub owner: manifold_core::GraphTarget,
    param_id: String,
    label: String,
    min: f32,
    max: f32,
    invert: bool,
    curve: manifold_core::macro_bank::MacroCurve,
    scale: f32,
    offset: f32,
    is_angle: bool,
    section: Option<String>,
    anchor: manifold_ui::graph_canvas::Rect,
    clip: manifold_ui::graph_canvas::Rect,
}

impl CardMapping {
    pub(crate) fn open(self, popover: &mut crate::mapping_popover::MappingPopover) {
        popover.open(
            crate::editing_host::to_ui_graph_target(&self.owner), self.param_id, self.label,
            self.min, self.max, self.invert, crate::ui_translate::macro_curve_to_ui(self.curve),
            self.scale, self.offset, None, self.is_angle, self.section, self.anchor, self.clip,
        );
    }
}

/// Resolves the mapping drawer an editor card row asks for: the shared target
/// resolver, the watched graph when it is the same host, and the clicked
/// row's geometry. `None` when the param has no reshape or the row is gone.
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_card_mapping(
    editor: &crate::ui_root::UIRoot,
    project: &manifold_core::project::Project,
    active_layer: &Option<manifold_core::LayerId>,
    selection: &crate::app::SelectionState,
    watched: Option<&manifold_core::GraphTarget>,
    target: &manifold_ui::GraphParamTarget,
    param_id: &str,
    anchor_node_id: manifold_ui::node::NodeId,
    clip: manifold_ui::graph_canvas::Rect,
) -> Option<CardMapping> {
    let resolved_owner = crate::ui_bridge::resolve_graph_target(
        target, None, editor.inspector.last_effect_tab(), active_layer, selection, project,
    )?;
    let owner = watched
        .filter(|watched| watched.host_target() == Some(&resolved_owner))
        .cloned()
        .unwrap_or(resolved_owner);
    let (label, min, max, invert, curve, scale, offset) = target_full_reshape(project, &owner, param_id)?;
    let param = project.graph_target_owner(&owner).and_then(|instance| instance.params.get(param_id));
    let is_angle = param.is_some_and(|param| param.spec.is_angle);
    let section = param.and_then(|p| p.spec.section.clone());
    editor.tree.get_node(anchor_node_id)?;
    let anchor = editor.tree.get_bounds(anchor_node_id);
    Some(CardMapping {
        owner,
        param_id: param_id.to_owned(),
        label, min, max, invert, curve, scale, offset, is_angle, section,
        anchor: manifold_ui::graph_canvas::Rect::new(anchor.x, anchor.y, anchor.width, anchor.height),
        clip,
    })
}

/// What a mapping action left for the caller.
pub(crate) enum MappingDispatch {
    NotMapping,
    Done,
    /// Navigate the canvas to the node this binding is exposed from
    /// ([`goto_mapped_node`]).
    GotoNode(manifold_core::GraphTarget, String),
}

/// Centres the canvas on the node a binding is exposed from. A snapshot
/// belongs to the watched graph, so a same-named binding in another graph is
/// never navigated.
pub(crate) fn goto_mapped_node(
    watched: Option<&manifold_core::GraphTarget>,
    snapshot: Option<&std::sync::Arc<manifold_ui::graph_view::GraphSnapshot>>,
    canvas: Option<&mut crate::graph_canvas::GraphCanvas>,
    target: &manifold_core::GraphTarget,
    binding_id: &str,
) {
    if watched == Some(target)
        && let Some(snapshot) = snapshot
        && let Some(node_id) = crate::graph_canvas::resolve_card_param_node_id(snapshot, binding_id)
        && let Some(canvas) = canvas
    {
        canvas.focus_node(snapshot, &node_id);
    }
}

/// What the mapping actions read and write. The App and the ui-snap runner
/// build one from their own state.
pub(crate) struct MappingHost<'a> {
    pub project: crate::ui_bridge::ProjectRef<'a>,
    pub scrub: &'a mut crate::ui_bridge::ScrubState,
}

impl MappingHost<'_> {
    /// Live-drag preview: apply the partial edit to the watched param's
    /// reshape store on BOTH the local project (immediate card UI) and the
    /// content thread (smooth canvas + survives the next snapshot sync),
    /// WITHOUT recording undo — the commit records the single undo entry.
    /// Reuses the edit command's own apply logic (it picks user-binding vs
    /// note + seeds copy-on-write), so the preview can never diverge from
    /// the commit.
    fn preview(&mut self, target: &manifold_core::GraphTarget, param_id: &str, edit: BindingMappingEdit) {
        let Some(target) = mapping_write_target(&self.project, target, param_id) else { return; };
        let seed_def = seed_def_for_project(&self.project, &target);
        // A reshape is a live scalar (Shapes, no reaction); the first tick may
        // also materialize the graph override (D21).
        let graph_absent = self
            .project
            .preset_instance(&target)
            .is_some_and(|inst| inst.graph.is_none());
        let pid = param_id.to_string();
        self.project.gesture_move(
            crate::content_command::touch::shapes_first_move(graph_absent),
            move |p| {
                build_mapping_command(&target, &pid, edit, seed_def).execute(p);
            },
        );
    }

    fn send(&self, cmd: ContentCommand) {
        ContentCommand::send(self.project.content_tx(), cmd);
    }

    /// Commit / single-shot: send the reshape edit as one undoable command.
    /// Self-captures the reverse (correct for single-shot — nothing mutated
    /// the store first).
    fn commit(&self, target: &manifold_core::GraphTarget, param_id: &str, edit: BindingMappingEdit) {
        let Some(target) = mapping_write_target(&self.project, target, param_id) else { return; };
        let seed_def = seed_def_for_project(&self.project, &target);
        self.send(ContentCommand::Execute(build_mapping_command(&target, param_id, edit, seed_def)));
    }

    /// Drag commit: one undoable command carrying the explicit pre-drag
    /// reverse, so undo restores the pre-drag values rather than the
    /// preview-mutated ones.
    fn commit_with_reverse(
        &self,
        target: &manifold_core::GraphTarget,
        param_id: &str,
        new: BindingMappingEdit,
        reverse: BindingMappingEdit,
    ) {
        let Some(target) = mapping_write_target(&self.project, target, param_id) else { return; };
        let seed_def = seed_def_for_project(&self.project, &target);
        self.send(ContentCommand::Execute(build_mapping_command_with_reverse(
            &target, param_id, new, reverse, seed_def,
        )));
    }

    /// The mapping action handler, shared by both modal entry points. Every
    /// event carries the owner captured at open; selection is irrelevant.
    pub(crate) fn dispatch(&mut self, action: &manifold_ui::RootAction) -> MappingDispatch {
        use manifold_ui::RootAction;
        use crate::ui_bridge::scrub::ResolvedScrub;
        match action {
            RootAction::EffectMappingRangeSnapshot { target, binding_id } => {
                let target = &crate::editing_host::to_graph_target(target);
                if let Some(shape) = target_full_reshape(&self.project, target, binding_id) {
                    self.scrub.check_single_active_on_begin("mapping-range");
                    self.scrub.active = Some(ResolvedScrub::MappingRange {
                        target: target.clone(), param_id: binding_id.clone(),
                        baseline: (shape.1, shape.2), live: (shape.1, shape.2),
                    });
                }
            }
            RootAction::EffectMappingRangeChanged { target, binding_id, min, max } => {
                let target = &crate::editing_host::to_graph_target(target);
                if let Some(ResolvedScrub::MappingRange { target: owner, param_id, live, .. }) = &mut self.scrub.active
                    && owner == target && param_id == binding_id
                {
                    *live = (*min, *max);
                    self.preview(target, binding_id, BindingMappingEdit {
                        min: Some(*min), max: Some(*max), ..Default::default()
                    });
                }
            }
            RootAction::EffectMappingRangeCommit { target, binding_id } => {
                let target = &crate::editing_host::to_graph_target(target);
                if matches!(&self.scrub.active,
                    Some(ResolvedScrub::MappingRange { target: owner, param_id, .. })
                    if owner == target && param_id == binding_id)
                    && let Some(ResolvedScrub::MappingRange { baseline, live, .. }) = self.scrub.active.take()
                    && ((baseline.0 - live.0).abs() > f32::EPSILON || (baseline.1 - live.1).abs() > f32::EPSILON)
                {
                    self.commit_with_reverse(target, binding_id,
                        BindingMappingEdit { min: Some(live.0), max: Some(live.1), ..Default::default() },
                        BindingMappingEdit { min: Some(baseline.0), max: Some(baseline.1), ..Default::default() },
                    );
                }
            }
            RootAction::EffectMappingAffineSnapshot { target, binding_id } => {
                let target = &crate::editing_host::to_graph_target(target);
                if let Some(shape) = target_full_reshape(&self.project, target, binding_id) {
                    self.scrub.check_single_active_on_begin("mapping-affine");
                    self.scrub.active = Some(ResolvedScrub::MappingAffine {
                        target: target.clone(), param_id: binding_id.clone(),
                        baseline: (shape.5, shape.6), live: (shape.5, shape.6),
                    });
                }
            }
            RootAction::EffectMappingAffineChanged { target, binding_id, scale, offset } => {
                let target = &crate::editing_host::to_graph_target(target);
                if let Some(ResolvedScrub::MappingAffine { target: owner, param_id, live, .. }) = &mut self.scrub.active
                    && owner == target && param_id == binding_id
                {
                    *live = (*scale, *offset);
                    self.preview(target, binding_id, BindingMappingEdit {
                        scale: Some(*scale), offset: Some(*offset), ..Default::default()
                    });
                }
            }
            RootAction::EffectMappingAffineCommit { target, binding_id } => {
                let target = &crate::editing_host::to_graph_target(target);
                if matches!(&self.scrub.active,
                    Some(ResolvedScrub::MappingAffine { target: owner, param_id, .. })
                    if owner == target && param_id == binding_id)
                    && let Some(ResolvedScrub::MappingAffine { baseline, live, .. }) = self.scrub.active.take()
                    && ((baseline.0 - live.0).abs() > f32::EPSILON || (baseline.1 - live.1).abs() > f32::EPSILON)
                {
                    self.commit_with_reverse(target, binding_id,
                        BindingMappingEdit { scale: Some(live.0), offset: Some(live.1), ..Default::default() },
                        BindingMappingEdit { scale: Some(baseline.0), offset: Some(baseline.1), ..Default::default() },
                    );
                }
            }
            RootAction::EffectMappingLabel { target, binding_id, label } => {
                let target = &crate::editing_host::to_graph_target(target);
                self.commit(target, binding_id, BindingMappingEdit {
                    label: Some(label.clone()), ..Default::default()
                });
            }
            RootAction::EffectMappingSection { target, binding_id, section } => {
                let target = &crate::editing_host::to_graph_target(target);
                self.commit(target, binding_id, BindingMappingEdit {
                    section: Some(section.clone()), ..Default::default()
                });
            }
            RootAction::EffectMappingInvert { target, binding_id, invert } => {
                let target = &crate::editing_host::to_graph_target(target);
                self.commit(target, binding_id, BindingMappingEdit {
                    invert: Some(*invert), ..Default::default()
                });
            }
            RootAction::EffectMappingCurve { target, binding_id, curve } => {
                let target = &crate::editing_host::to_graph_target(target);
                self.commit(target, binding_id, BindingMappingEdit {
                    curve: Some(crate::ui_translate::macro_curve_to_core(*curve)), ..Default::default()
                });
            }
            RootAction::EffectMappingGotoNode { target, binding_id } => {
                return MappingDispatch::GotoNode(crate::editing_host::to_graph_target(target), binding_id.clone());
            }
            RootAction::EffectMappingCancel { target, binding_id } => {
                let target = &crate::editing_host::to_graph_target(target);
                let reverse = match &self.scrub.active {
                    Some(ResolvedScrub::MappingRange { target: owner, param_id, baseline, .. })
                        if owner == target && param_id == binding_id =>
                        Some(BindingMappingEdit { min: Some(baseline.0), max: Some(baseline.1), ..Default::default() }),
                    Some(ResolvedScrub::MappingAffine { target: owner, param_id, baseline, .. })
                        if owner == target && param_id == binding_id =>
                        Some(BindingMappingEdit { scale: Some(baseline.0), offset: Some(baseline.1), ..Default::default() }),
                    _ => None,
                };
                if let Some(reverse) = reverse {
                    self.scrub.active = None;
                    self.preview(target, binding_id, reverse);
                }
            }
            _ => return MappingDispatch::NotMapping,
        }
        MappingDispatch::Done
    }
}

impl Application {
    /// Window-hosted actions shared by both instances of the inspector.
    /// The action already captures its parameter/mask identity; only the text
    /// overlay and keyboard ownership depend on the emitting window.
    pub(crate) fn dispatch_inspector_host_action(
        &mut self,
        action: &manifold_ui::panels::PanelAction,
        in_editor: bool,
    ) -> bool {
        let tab = match self.graph_editor.as_ref() {
            Some(ed) if in_editor => ed.ui_root.inspector.last_effect_tab(),
            _ => self.ws.ui_root.inspector.last_effect_tab(),
        };
        // An editor-owned field is a live surface of the editor's composite
        // while it is open, so opening one needs no repaint call.
        crate::text_session::begin_inspector_entry(
            &mut self.text_input,
            action,
            in_editor,
            tab,
            &self.active_layer_id,
            &self.selection,
            &self.local_project,
        )
    }

    /// Open from the emitting card using the shared target resolver and its
    /// clicked node geometry. Canvas selection is not an input.
    pub(crate) fn open_card_mapping(
        &mut self,
        target: &manifold_ui::GraphParamTarget,
        param_id: &str,
        anchor_node_id: manifold_ui::node::NodeId,
        clip: manifold_ui::graph_canvas::Rect,
    ) -> bool {
        let Some(ed) = self.graph_editor.as_ref() else { return false; };
        let Some(open) = resolve_card_mapping(
            &ed.ui_root, &self.local_project, &self.active_layer_id, &self.selection,
            self.watched_graph_target.as_ref(), target, param_id, anchor_node_id, clip,
        ) else { return false; };
        // Modifiers have no card selection-follow action. Bring their graph
        // into view through the same watched-target path as other cards.
        if self.watched_graph_target.as_ref() != Some(&open.owner) {
            self.watch_graph_target(open.owner.clone());
        }
        open.open(&mut self.editor_mapping_popover);
        true
    }

    /// The mapping action handler both modal entry points share
    /// ([`MappingHost::dispatch`]). False when `action` is not a mapping action.
    pub(crate) fn dispatch_mapping_action(&mut self, action: &manifold_ui::RootAction) -> bool {
        let Some(mut host) = self.mapping_host() else { return false; };
        let outcome = host.dispatch(action);
        if let MappingDispatch::GotoNode(target, binding_id) = &outcome {
            let snapshot = self.editor_ui_snapshot();
            goto_mapped_node(self.watched_graph_target.as_ref(), snapshot.as_ref(), self.graph_canvas.as_mut(), target, binding_id);
        }
        !matches!(outcome, MappingDispatch::NotMapping)
    }

    fn mapping_host(&mut self) -> Option<MappingHost<'_>> {
        Some(MappingHost {
            project: self.local_project.edit(&mut self.ui_edits, self.content_tx.as_ref()?),
            scrub: &mut self.scrub,
        })
    }

    /// Cursor position over the audio scope's waterfall, if inside it:
    /// `(uv_x, uv_y, freq_hz)` with uv in 0..1 (y top→bottom) and the frequency
    /// at that height on the log axis. `None` when the cursor is elsewhere, the
    /// scope is closed, or the analysed range is degenerate.
    pub(crate) fn scope_hover_uv(&self) -> Option<(f32, f32, f32)> {
        let rect = self.ws.ui_root.audio_setup_panel.scope_rect()?;
        if !rect.contains(self.cursor_pos) || rect.width <= 0.0 || rect.height <= 0.0 {
            return None;
        }
        let fmin = self.content_state.spectrogram_fmin;
        let fmax = self.content_state.spectrogram_fmax;
        if !(fmin > 0.0 && fmax > fmin) {
            return None;
        }
        let ux = ((self.cursor_pos.x - rect.x) / rect.width).clamp(0.0, 1.0);
        let uy = ((self.cursor_pos.y - rect.y) / rect.height).clamp(0.0, 1.0);
        // uv.y=1 (bottom) → fmin, uv.y=0 (top) → fmax; freq is geometric in height.
        let freq = fmin * (fmax / fmin).powf(1.0 - uy);
        Some((ux, uy, freq))
    }

    /// The card binding (if any) governing `(node_id, param_name)` on the
    /// watched graph at `scope_path` — BUG-158 write-back's binding lookup
    /// (D1). Same override-then-canonical fallback as
    /// [`target_full_reshape`]: most bound params are still on the
    /// bundled/canonical def (never diverged), so the instance's own
    /// per-instance `graph` override alone would miss them.
    pub(crate) fn watched_binding_for_node_param(
        &self,
        scope_path: &[u32],
        node_id: u32,
        param_name: &str,
    ) -> Option<(
        String,
        f32,
        f32,
        bool,
        manifold_core::macro_bank::MacroCurve,
        f32,
        f32,
    )> {
        if let Some(target) = self.watched_graph_target.as_ref()
            && matches!(target, manifold_core::GraphTarget::SceneModifier { .. })
        {
            return modifier_binding_for_node_param(
                &self.local_project, target, scope_path, node_id, param_name,
            );
        }
        let instance = self.local_project.preset_instance(self.watched_graph_target.as_ref()?)?;
        if let Some(def) = instance.graph.as_ref() {
            return binding_for_node_param(&instance.params, def, scope_path, node_id, param_name);
        }
        let view = manifold_node_engine::load::loaded_preset_view::loaded_preset_view_by_id(instance.effect_type())?;
        binding_for_node_param(&instance.params, view.canonical_def.as_ref(), scope_path, node_id, param_name)
    }

    /// BUG-282: the current value of `(node_id, param_name)` on the watched
    /// graph at `scope_path`, before any drag write touches it — the
    /// pre-drag undo baseline for an UNBOUND node-face scrub. `catalog_default`
    /// is the same fallback `SetGraphNodeParamCommand` itself uses when the
    /// instance has no per-instance `graph` override yet
    /// (`with_target_graph_mut`'s `get_or_insert_with`), so this mirrors
    /// exactly what `execute()` would read/self-capture if it ran right now.
    pub(crate) fn watched_current_node_param_value(
        &self,
        scope_path: &[u32],
        node_id: u32,
        param_name: &str,
        catalog_default: &manifold_core::effect_graph_def::EffectGraphDef,
    ) -> Option<manifold_core::effect_graph_def::SerializedParamValue> {
        let target = self.watched_graph_target.as_ref()?;
        let def = self.local_project.graph_for_target(target, Some(catalog_default))?;
        node_param_value(def, scope_path, node_id, param_name)
    }

    /// `true` when `(node_id, param_name)` on the watched graph at
    /// `scope_path` has an incoming wire — same override-then-canonical
    /// fallback as [`Self::watched_binding_for_node_param`]. The P1
    /// enforcement backstop (Invariants section 4): a wired param is never rerouted
    /// through the card write-back path, matching D5/D6 (wire beats binding).
    pub(crate) fn watched_node_param_is_wired(&self, scope_path: &[u32], node_id: u32, param_name: &str) -> bool {
        let Some(target) = self.watched_graph_target.as_ref() else {
            return false;
        };
        crate::graph_target::resolve(&self.local_project, target)
            .is_some_and(|def| node_param_is_wired(&def, scope_path, node_id, param_name))
    }

    /// The graph def the editor currently shows for the watched target — the
    /// per-instance override if it has diverged, else the catalog / bundled
    /// default. Cloned (copy is a rare authoring action). `None` when nothing is
    /// watched.
    pub(crate) fn watched_def_cloned(&self) -> Option<manifold_core::effect_graph_def::EffectGraphDef> {
        crate::graph_target::resolve(&self.local_project, self.watched_graph_target.as_ref()?)
            .map(|graph| (*graph).clone())
    }

    /// Authoring values for viewport geometry, including exposed controls.
    /// This is a disposable projection, never a project edit or runtime result.
    pub(crate) fn viewport_def_cloned(&self) -> Option<manifold_core::effect_graph_def::EffectGraphDef> {
        crate::scene_gizmo::authored_def(&self.local_project, self.watched_graph_target.as_ref()?)
    }

    /// Resolve the canvas's current selection into copy-ready data: the selected
    /// def nodes plus the wires whose BOTH endpoints are selected (internal
    /// connectivity only). `None` when nothing is watched or selected. Backs
    /// Cmd+C (store to clipboard) and Cmd+D (duplicate immediately).
    pub(crate) fn copy_selected_graph_nodes(
        &self,
    ) -> Option<(
        Vec<manifold_core::effect_graph_def::EffectGraphNode>,
        Vec<manifold_core::effect_graph_def::EffectGraphWire>,
    )> {
        let canvas = self.graph_canvas.as_ref()?;
        let ids: std::collections::HashSet<u32> = canvas.selected_ids().into_iter().collect();
        if ids.is_empty() {
            return None;
        }
        let scope = canvas.scope_path().to_vec();
        let def = self.watched_def_cloned()?;
        let (nodes, wires) = descend_def_level(&def, &scope)?;
        let sel_nodes: Vec<_> = nodes.iter().filter(|n| ids.contains(&n.id)).cloned().collect();
        if sel_nodes.is_empty() {
            return None;
        }
        let sel_wires: Vec<_> = wires
            .iter()
            .filter(|w| ids.contains(&w.from_node) && ids.contains(&w.to_node))
            .cloned()
            .collect();
        Some((sel_nodes, sel_wires))
    }

    /// Modal Yes/No confirm shown before deleting a graph node that backs card
    /// sliders. Lists the controls the delete would remove so the choice is
    /// informed. Returns true only on Yes. Blocking native dialog — fine for an
    /// authoring-time action (same as the preset import/export dialogs); never
    /// reached during performance.
    pub(crate) fn confirm_remove_node_orphans(labels: &[String]) -> bool {
        let n = labels.len();
        let list = labels
            .iter()
            .map(|l| format!("  •  {l}"))
            .collect::<Vec<_>>()
            .join("\n");
        let msg = format!(
            "Deleting this node will also remove {n} card control{}:\n\n{list}\n\nThis can be undone.",
            if n == 1 { "" } else { "s" },
        );
        rfd::MessageDialog::new()
            .set_title("Remove node")
            .set_description(msg)
            .set_buttons(rfd::MessageButtons::YesNo)
            .set_level(rfd::MessageLevel::Warning)
            .show()
            == rfd::MessageDialogResult::Yes
    }

    /// Resolve an effect card's row index (in the active inspector tab) to its
    /// stable [`EffectId`]. Mirrors the cog's `OpenGraphEditor` resolution:
    /// keyed by instance id, tab-aware (Master / Layer|Group / Clip).
    pub(crate) fn resolve_effect_card_id(&self, ei: usize) -> Option<manifold_core::EffectId> {
        effect_card_id(
            self.ws.ui_root.inspector.last_effect_tab(),
            &self.local_project,
            self.active_layer_id.as_ref(),
            &self.selection,
            ei,
        )
    }

    /// Point the graph editor at an effect instance: push the watched identity
    /// to the content thread and cache its catalog-default graph def (so the
    /// per-card edit commands can lift `instance.graph` from `None` on first
    /// edit). Does NOT open the window — shared by the card cog (which also
    /// sets `pending_open_graph_editor`) and selection-follows (retarget-only).
    pub(crate) fn watch_effect_graph(&mut self, effect_id: manifold_core::EffectId) {
        self.watch_graph_target(manifold_core::GraphTarget::Effect(effect_id));
    }

    pub(crate) fn watch_generator_graph(&mut self, layer_id: manifold_core::LayerId) {
        self.watch_graph_target(manifold_core::GraphTarget::Generator(layer_id));
    }

    pub(crate) fn watch_graph_target(&mut self, target: manifold_core::GraphTarget) {
        if !watch_editor_graph(
            &self.local_project,
            &target,
            &mut self.watched_graph_target,
            &mut self.watched_catalog_default,
            &mut self.editor_mapping_popover,
            self.graph_canvas.as_mut(),
            self.content_tx.as_ref(),
        ) {
            return;
        }
        if self.modifier_preview_object.as_ref().is_some_and(|(owner, _)| owner != &target) {
            self.modifier_preview_object = None;
        }
        self.editor_graph.catalog_default_changed();
        // The content thread clears both preview requests when the watched
        // target changes; drop the dedup caches with it so a same-named node
        // in the next graph is sent again rather than leaving stale output.
        self.last_preview_node = None;
        self.last_modifier_preview_context = None;
        self.last_modifier_preview_selection = None;
    }

    /// Open the shared Save to Library / Save to Project name-prompt text
    /// input (PRESET_LIBRARY_DESIGN D4, P3). Shared tail for the card-menu
    /// path (`PanelAction::SaveToLibrary`/`SaveToProject`, resolved by
    /// `dispatch_inspector` and handed back via `DispatchResult`) — the
    /// graph editor header path (`GraphEditCommand::SaveGraphToLibrary`/
    /// `SaveGraphToProject`) opens the session inline instead, since it
    /// already has a real button rect to anchor on; this path anchors on the
    /// dropdown's last position (cosmetic only — a degenerate rect just
    /// anchors near the top-left, which doesn't affect the save itself).
    pub(crate) fn begin_save_preset_prompt(
        &mut self,
        kind: manifold_core::preset_def::PresetKind,
        def: manifold_core::effect_graph_def::EffectGraphDef,
        destination: crate::text_input::SavePresetDestination,
    ) {
        let bounds = self.ws.ui_root.dropdown.container_bounds();
        let anchor = if bounds.width > 0.0 && bounds.height > 0.0 {
            crate::text_input::AnchorRect::new(bounds.x, bounds.y, bounds.width, 24.0)
        } else {
            crate::text_input::AnchorRect::new(120.0, 120.0, 220.0, 24.0)
        };
        self.text_input.begin(crate::text_input::TextInputField::SavePresetName, "", anchor, 12.0);
        self.text_input.save_preset = Some(crate::text_input::SavePresetCtx { kind, def, destination });
    }

    /// Open the browser management-menu Rename prompt (PRESET_LIBRARY_DESIGN
    /// P5, D6) — same shape as [`Self::begin_save_preset_prompt`] (panel-
    /// owned, not `begin_owned`: by the time this runs, the dropdown that
    /// offered "Rename…" has already closed itself on selection, so tagging
    /// this session with that overlay's id would have it cancelled the very
    /// next frame's closed-overlay drain).
    pub(crate) fn begin_rename_preset_prompt(
        &mut self,
        kind: manifold_core::preset_def::PresetKind,
        id: manifold_core::PresetTypeId,
        source: manifold_ui::panels::picker_core::Source,
        initial_name: String,
    ) {
        let bounds = self.ws.ui_root.dropdown.container_bounds();
        let anchor = if bounds.width > 0.0 && bounds.height > 0.0 {
            crate::text_input::AnchorRect::new(bounds.x, bounds.y, bounds.width, 24.0)
        } else {
            crate::text_input::AnchorRect::new(120.0, 120.0, 220.0, 24.0)
        };
        self.text_input.begin(
            crate::text_input::TextInputField::RenamePreset,
            &initial_name,
            anchor,
            12.0,
        );
        self.text_input.rename_preset = Some(crate::text_input::RenamePresetCtx { kind, id, source });
    }

    /// Render and present one frame to the graph editor window.
    ///
    /// Renders to the editor's offscreen via `UIRenderer` (clear + a
    /// centered "Graph Editor" placeholder label) and blits the result
    /// to the drawable. Phase 4 replaces the placeholder with a real
    /// `GraphCanvasPanel`.
    ///
    /// Gated on the editor's own CVDisplayLink: when it hasn't fired,
    /// we skip the present to avoid wasting a drawable slot.
    /// The graph-editor canvas viewport rect in logical pixels — the same
    /// region `canvas.render` draws into. `None` when the editor window is
    /// closed or its surface isn't ready. Used to anchor overlays (the group
    /// rename field) in screen space from the key handler, outside the present
    /// pass where `canvas_x`/`canvas_width` are computed inline.
    pub(crate) fn editor_canvas_viewport(&self) -> Option<crate::graph_canvas::Rect> {
        let wid = self.graph_editor_window_id?;
        let win_state = self.window_registry.get(&wid)?;
        let surface = win_state.surface.as_ref()?;
        let scale = win_state.window.scale_factor();
        let logical_w = (surface.width as f64 / scale).max(1.0) as f32;
        let logical_h = (surface.height as f64 / scale).max(1.0) as f32;
        // Same `Dock` the present pass reads, so this mapping viewport tracks
        // whatever the user dragged the columns to.
        let area = manifold_ui::Rect::new(0.0, 0.0, logical_w, logical_h);
        let c = self.graph_editor.as_ref()?.dock.canvas(area);
        Some(crate::graph_canvas::Rect::new(c.x, c.y, c.width, c.height))
    }

    /// The current editor graph as the UI-local view-model the canvas reads,
    /// translating (and caching) the renderer snapshot on change. Returns an
    /// owned `Arc` (no `self` borrow held), so callers can use it alongside a
    /// `&mut self.graph_canvas` borrow. `None` when no graph is active.
    pub(crate) fn editor_ui_snapshot(
        &mut self,
    ) -> Option<std::sync::Arc<manifold_ui::graph_view::GraphSnapshot>> {
        editor_ui_graph(&mut self.editor_ui_graph, &self.content_state, self.watched_graph_target.as_ref(), &self.local_project)
    }

    pub(crate) fn present_graph_editor_window(&mut self, dt: f32) {
        // Forward the editor's single-node selection to the content thread so
        // it can capture that node's output for the preview pane. Deduplicated
        // against the last send. A closed editor (`graph_canvas == None`)
        // yields `None`, clearing the preview.
        // Resolve the single selection to a preview target. A boundary node
        // (Group Input/Output, final output) has no runtime instance of its
        // own, so resolve it against the hierarchical snapshot at the canvas
        // scope to the concrete node whose texture stands in for that boundary.
        // The UI-local graph view-model the canvas + these editor helpers read
        // (Phase 8). Translated once (cached by Arc identity); the renderer
        // snapshot stays the source for the binding/exposure helpers below.
        let editor_ui_snap = self.editor_ui_snapshot();
        let scene_modifier_watched = matches!(
            self.watched_graph_target.as_ref(),
            Some(manifold_core::GraphTarget::SceneModifier { .. })
        );
        let preview_object = self.modifier_preview_object.as_ref()
            .filter(|(target, _)| Some(target) == self.watched_graph_target.as_ref())
            .map(|(_, object)| object.clone());
        let (preview_node, modifier_context) = match (self.graph_canvas.as_ref(), editor_ui_snap.as_ref()) {
            (Some(canvas), Some(snap)) if scene_modifier_watched => {
                let selected = canvas.selected_node_id();
                let canvas_scope = canvas.scope_path();
                let unchanged = self.last_modifier_preview_selection.as_ref().is_some_and(
                    |(cached_snap, cached_scope, cached_selected)| {
                        std::sync::Arc::ptr_eq(cached_snap, snap)
                            && cached_scope.as_slice() == canvas_scope
                            && *cached_selected == selected
                    },
                );
                if unchanged {
                    (self.last_preview_node.clone(), None)
                } else {
                    let (node, scope) = modifier_preview_selection(snap, canvas_scope, selected);
                    self.last_modifier_preview_selection = Some((snap.clone(), canvas_scope.to_vec(), selected));
                    (node, Some((scope, preview_object)))
                }
            }
            (Some(canvas), Some(snap)) => (
                canvas
                    .selected_node_id()
                    .and_then(|id| resolve_preview_target(snap, canvas.scope_path(), id)),
                None,
            ),
            _ if scene_modifier_watched => {
                self.last_modifier_preview_selection = None;
                (None, Some((Vec::new(), None)))
            }
            _ => {
                self.last_modifier_preview_selection = None;
                (None, None)
            }
        };
        if preview_node != self.last_preview_node {
            if let Some(tx) = self.content_tx.as_ref() {
                crate::content_command::ContentCommand::send(
                    tx,
                    crate::content_command::ContentCommand::SetGraphPreviewNode(
                        preview_node.clone(),
                    ),
                );
            }
            self.last_preview_node = preview_node;
        }
        if let Some(context) = modifier_context
            && self.last_modifier_preview_context.as_ref() != Some(&context) {
                let (scope, object) = context.clone();
                self.send_content_cmd(ContentCommand::SetModifierPreviewContext { scope, object });
                self.last_modifier_preview_context = Some(context);
        }

        // Scene navigation requests go to the live runtime. The authored
        // definition below is used only for scene selection and edit gizmos;
        // camera and accepted bounds come from the displayed frame.
        let viewport_is_scene_node = self.last_preview_node.as_ref().is_some_and(|id| {
            self.content_state
                .active_graph_snapshot
                .as_deref()
                .and_then(|s| find_snapshot_node(&s.nodes, id))
                .is_some_and(|n| n.type_id == "node.render_scene")
        });
        let viewport_open = self.graph_editor.as_ref().is_some_and(|ed| ed.viewport_open);
        let viewport_def = if viewport_is_scene_node && viewport_open {
            self.editor_graph.viewport_def(&self.local_project, self.watched_graph_target.as_ref(), &self.ui_edits.stamps.parts())
        } else {
            crate::editor_graph_cache::ViewportDef::default()
        };

        // Send the canvas's currently-visible nodes (deduped) so the content
        // thread captures thumbnails only for what's on screen — hidden /
        // off-scope / collapsed-group nodes cost nothing. The set is the whole
        // current scope level (what the atlas shows), so it changes only on a
        // scope descend/ascend or a topology edit, not on pan/zoom.
        // Keyed on the snapshot it reads (by Arc identity) and the scope path:
        // a steady frame walks and allocates nothing (P4c).
        let same_snap = match (&editor_ui_snap, &self.atlas_visible_snap) {
            (Some(a), Some(b)) => std::sync::Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        let scope: &[u32] = self.graph_canvas.as_ref().map_or(&[], |c| c.scope_path());
        if !same_snap || scope != self.atlas_visible_scope.as_slice() {
            self.atlas_visible_snap = editor_ui_snap.clone();
            self.atlas_visible_scope.clear();
            self.atlas_visible_scope.extend_from_slice(scope);
            manifold_core::work_count::record_work(manifold_core::work_count::WorkKind::EditorProjection);
            let visible_nodes: Vec<manifold_core::NodeId> = match (
                self.graph_canvas.as_ref(),
                editor_ui_snap.as_deref(),
            ) {
                (Some(canvas), Some(snap)) => {
                    let (nodes, _) =
                        crate::graph_canvas::resolve_level(snap, canvas.scope_path())
                            .unwrap_or((&snap.nodes, &snap.wires));
                    nodes
                        .iter()
                        .filter_map(crate::graph_canvas::node_preview_target)
                        .collect()
                }
                _ => Vec::new(),
            };
            if visible_nodes != self.last_atlas_visible_sent {
                self.send_content_cmd(
                    crate::content_command::ContentCommand::SetNodeAtlasVisible(visible_nodes.clone()),
                );
                self.last_atlas_visible_sent = visible_nodes;
            }
        }

        // ── Mini-timeline data ────────────────────────────────────────
        // Built from the UI project mirror + playhead BEFORE the disjoint `ws`
        // borrow below. Cheap: walks the clip list once, off the per-node hot
        // path. Shared with the headless snapshot path via `mini_timeline_data`;
        // keyed so a steady frame rebuilds nothing.
        let mini_current_beat = self.content_state.current_beat.as_f32();
        let mini_is_playing = self.content_state.is_playing;
        let parts = self.ui_edits.stamps.parts();
        self.mini_timeline.get(&self.local_project, mini_current_beat, &parts);
        let mini_key = self.mini_timeline.key();
        let &(ref mini_clips, ref mini_layer_labels, mini_rows, mini_total_beats, mini_beats_per_bar, ref mini_readout) =
            self.mini_timeline.data();

        let Some(gpu) = &self.gpu else { return };
        let Some(wid) = self.graph_editor_window_id else {
            return;
        };
        // Resolve the open popover's live value before borrowing the editor
        // window state mutably. Its owner may differ from the watched graph.
        let popover_live_value = if self.editor_mapping_popover.is_open() {
            self.editor_mapping_popover.target()
                .and_then(|target| self.local_project.graph_target_owner(&crate::editing_host::to_graph_target(target)))
                .and_then(|instance| instance.params.get(self.editor_mapping_popover.binding_id()))
                .map(|param| param.value)
        } else {
            None
        };
        let Some(ws) = self.graph_editor.as_mut() else {
            return;
        };

        // Consume editor vsync signal — skip when no pulse fired.
        // (Falls through to render when there's no display link, e.g.
        // non-macOS.)
        #[cfg(target_os = "macos")]
        {
            let pulse = ws
                .ui_display_link
                .as_ref()
                .is_none_or(|dl| dl.vsync_ready());
            if !pulse {
                return;
            }
        }

        let Some(win_state) = self.window_registry.get(&wid) else {
            return;
        };
        let Some(surface) = win_state.surface.as_ref() else {
            return;
        };
        let scale = win_state.window.scale_factor();
        let (surface_w, surface_h) = (surface.width, surface.height);

        let Some(offscreen) = ws.ui_offscreen.as_ref() else {
            return;
        };
        // Surface/offscreen size mismatch: a resize is in flight. Skip
        // until the matching `resize_graph_editor_offscreen()` lands.
        if offscreen.width != surface_w || offscreen.height != surface_h {
            return;
        }

        let logical_w = (surface_w as f64 / scale).max(1.0) as u32;
        let logical_h = (surface_h as f64 / scale).max(1.0) as u32;

        // ── Editor window layout ──────────────────────────────────────
        // One `EditorLayout` (the workspace's resizable `Dock` plus the two
        // stacked monitors) feeds the tree build, the composite and the
        // monitor blits; the pointer handlers read the same `Dock`, so
        // hit-testing tracks whatever the user dragged the columns to.
        let output_aspect = self
            .content_pipeline_output
            .as_ref()
            .map(|p| p.get_dimensions())
            .filter(|(_, h)| *h > 0)
            .map(|(w, h)| w as f32 / h as f32);
        let layout = crate::editor_frame::EditorLayout::compute(&ws.dock, logical_w, logical_h, output_aspect);
        let monitor_aspect = layout.monitor_aspect;
        let canvas_x = layout.canvas.x;
        let canvas_width = layout.canvas.width;
        let (preview_x, preview_w, preview_h) = (layout.preview_x, layout.preview_w, layout.preview_h);
        let (node_img_y, master_img_y) = (layout.node_img_y, layout.master_img_y);
        // The node pane's body shows the selected node's image, or its value
        // inspector text for a node with no image, or a placeholder; the
        // master pane always shows the live output.
        let mut node_preview_info = self.content_state.node_preview_info.clone();
        let preview_has_image = node_preview_info
            .as_ref()
            .map(|i| i.has_image)
            .unwrap_or(false);
        let mut show_image = self.last_preview_node.is_some() && preview_has_image;

        #[cfg(target_os = "macos")]
        let mut bridge_leases: crate::shared_texture::BridgeLeaseBatch = Vec::new();
        // The leased viewport frame's selection mask, for the outline (D-102).
        #[cfg(target_os = "macos")]
        let mut viewport_mask: Option<manifold_gpu::GpuTexture> = None;
        let viewport_scale = scale as f32;
        let viewport_tex_w = ((preview_w * viewport_scale).round() as u32).clamp(64, 4096);
        let viewport_tex_h = ((preview_h * viewport_scale).round() as u32).clamp(64, 4096);
        if viewport_def.resolved
            && let Some(target) = self.watched_graph_target.as_ref()
            && let Some(node) = self.last_preview_node.as_ref()
        {
            if !ws.viewport_target.as_ref().is_some_and(|(owner, render)| owner == target && render == node)
                || ws.viewport_session.is_none()
            {
                // Opening the editor's viewport ends the output preview's
                // navigation (D7a).
                if self.ws.preview_navigation.end()
                    && let Some(tx) = self.content_tx.as_ref()
                {
                    ContentCommand::send(tx, ContentCommand::SetSceneViewport(None));
                }
                ws.viewport_session = Some(crate::scene_viewport::SceneViewportNavigation::new(
                    crate::scene_viewport::ViewportHost::GraphEditor, viewport_tex_w, viewport_tex_h));
                ws.viewport_target = Some((target.clone(), node.clone()));
            }
            let session = ws.viewport_session.as_mut().expect("navigation was created above");
            session.config.width = viewport_tex_w;
            session.config.height = viewport_tex_h;
            let request = session.request(target, node, self.last_modifier_preview_context.as_ref(), ws.viewport_request_sent.as_ref());
            if !ws.viewport_request_sent.as_ref().is_some_and(|sent| std::sync::Arc::ptr_eq(sent, &request)) {
                if let Some(tx) = self.content_tx.as_ref() {
                    ContentCommand::send(tx, ContentCommand::SetSceneViewport(Some(request.clone())));
                }
                ws.viewport_request_sent = Some(request.clone());
            }
            session.displayed = None;
            #[cfg(target_os = "macos")]
            if let Some(pane) = self.scene_viewport_pane.as_mut()
                && let Some((frame, texture, slot)) = crate::scene_viewport::lease_frame(
                    pane, &gpu.device, &self.content_state.scene_viewport_frames, &request, &mut bridge_leases)
            {
                viewport_mask = crate::selection_outline::leased_mask(self.outline_viewport_pane.as_mut(), &gpu.device, slot, frame.outline);
                if let Some(texture) = texture
                    && let Some(ui) = self.ui_renderer.as_mut()
                {
                    ui.register_external_texture(crate::scene_viewport::texture_handle(), texture);
                }
                session.displayed = Some(frame);
            }
        } else {
            ws.viewport_session = None;
            ws.viewport_target = None;
            if ws.viewport_request_sent.take().is_some()
                && let Some(tx) = self.content_tx.as_ref()
            {
                ContentCommand::send(tx, ContentCommand::SetSceneViewport(None));
            }
        }
        ws.viewport_rect = ws.viewport_session.is_some()
            .then(|| manifold_ui::Rect::new(preview_x, node_img_y, preview_w, preview_h));
        ws.viewport_overlay_lines.clear();
        ws.viewport_outline.clear();
        if let Some(session) = ws.viewport_session.as_ref() {
            show_image = session.displayed.as_ref().is_some_and(|frame| frame.has_image());
            if let Some(node) = self.last_preview_node.as_ref() {
                let info = node_preview_info.get_or_insert_with(|| crate::content_state::NodePreviewInfo {
                    node_id: node.clone(), has_image: false, diagnostic: None,
                    inputs: Vec::new(), outputs: Vec::new(),
                });
                info.has_image = show_image;
                info.diagnostic = session.displayed.as_ref().map_or(Some("Waiting for the scene to render…"), |frame| frame.diagnostic());
            }
            // The selected shape's handles over THIS frame's scene, through
            // the one composition entry the main preview also calls; mid-drag
            // they follow the live knob values the drag writes. Domain boxes
            // are the ones the solver accepted for the displayed frame.
            use manifold_nodes_scene::node_graph::viewport_gizmo::{GizmoChrome, compose_lines};
            let drawn = crate::viewport_selection::ViewportSelection::refresh(&mut ws.viewport_selection, viewport_def.scene.as_ref(), &ws.viewport_selected);
            if let Some(drawn) = drawn {
                drawn.outline(&mut ws.viewport_outline);
            }
            let displayed = session.displayed.as_ref().filter(|frame| frame.has_image());
            let shown = displayed.map(|frame| &frame.domains);
            let config = session.displayed_config().unwrap_or(session.config);
            ws.viewport_problems.update(viewport_def.scene.as_ref(), shown);
            compose_lines(
                &GizmoChrome {
                    view: crate::scene_gizmo::GizmoView::new(config.camera.to_camera(), config.width, config.height),
                    lights: &[],
                    domains: shown.map_or(&[][..], |domains| &domains[..]),
                    live_columns: displayed.map_or(&[][..], |frame| &frame.live_columns[..]),
                    physics: &[],
                    problems: ws.viewport_problems.problems(),
                    selection: drawn.map_or(&crate::physics_overlay::NO_SELECTION, |drawn| &drawn.selection),
                    mode: ws.viewport_gizmo_mode,
                    fade_handles: false,
                },
                &mut ws.viewport_overlay_lines,
            );
        }

        // The graph-editor panel is now just the node-output inspector + the
        // Smart-preview toggle (all param authoring moved onto the node face in
        // the canvas), so it needs only the preview state + the value-inspector
        // for a non-image node. `snap_arc` still feeds the value inspector's node
        // title lookup and the effect card below.
        let snap_arc = self.content_state.active_graph_snapshot.as_ref().cloned();
        self.graph_editor_panel
            .set_node_preview_normalize(self.node_preview_normalize);
        // Value inspector for a previewed node with no image: its description
        // (from the descriptor) + the live scalar I/O captured this frame.
        // The strings follow the node, its diagnostic and the snapshot; while
        // those hold, a frame only moves the port values in place.
        let shown = node_preview_info.as_ref().filter(|i| !i.has_image);
        let keep = shown.is_some_and(|info| {
            self.node_inspector_key
                .as_ref()
                .is_some_and(|key| key.matches(info, snap_arc.as_ref()))
                && self.graph_editor_panel.update_node_inspector_io(&info.inputs, &info.outputs)
        });
        self.node_inspector_key = shown.map(|info| NodeInspectorKey::of(info, snap_arc.as_ref()));
        let node_inspector = shown
            .filter(|_| !keep)
            .map(|info| {
                let snap_node = snap_arc
                    .as_deref()
                    .and_then(|s| find_snapshot_node(&s.nodes, &info.node_id));
                let title = snap_node
                    .map(|n| n.title.clone())
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| info.node_id.to_string());
                let description = info.diagnostic
                    .map(str::to_owned)
                    .or_else(|| {
                        snap_node
                            .and_then(|n| manifold_node_engine::descriptor::descriptor_for(&n.type_id))
                            .map(|d| {
                                if !d.summary.is_empty() {
                                    d.summary.to_string()
                                } else {
                                    // First sentence of the technical purpose keeps it short.
                                    d.purpose.split(". ").next().unwrap_or(d.purpose).to_string()
                                }
                            })
                    })
                    .unwrap_or_default();
                manifold_ui::panels::graph_editor::NodeInspector {
                    title,
                    description,
                    inputs: info.inputs.clone(),
                    outputs: info.outputs.clone(),
                }
            });
        if !keep {
            self.graph_editor_panel.set_node_inspector(node_inspector);
        }

        // Right lane = the whole main-window inspector column, this window's
        // own `ws.ui_root.inspector` instance, configured in the main tick in
        // lockstep with the main window's (docs/GRAPH_EDITOR_INSPECTOR_UNIFICATION.md
        // Change 3). The tree build is shared with the ui-snap script runner
        // (`editor_frame.rs`, BUG-12iyh (UI runner drives editor window,
        // perform mode and text input)).
        let editor_active_idx = self
            .active_layer_id
            .as_ref()
            .and_then(|id| self.local_project.timeline.find_layer_index_by_id(id));
        let editor_out = crate::editor_frame::run_editor_frame(
            &mut ws.ui_root,
            &mut ws.editor_frame,
            &self.graph_editor_panel,
            self.graph_canvas.as_mut(),
            &layout,
            crate::editor_frame::EditorFrameInputs {
                project: &self.local_project,
                content_state: &self.content_state,
                active_layer: editor_active_idx,
                live: self.frame_planner.last.live(),
                dt,
                show_image,
            },
            false,
        );
        self.editor_smart_preview_toggle_id = editor_out.smart_preview_toggle;
        ws.offscreen_dirty |= editor_out.redraw;

        // ── Node output previews (BUG-027): register the current atlas front
        // under a fixed handle and hand each visible node its cell UV, so the
        // canvas paints the preview inline at the node's own depth band (where a
        // node stacked above occludes it) instead of the old flat post-pass blit
        // that ignored node z-order. Phase A reads (build the map + clone the
        // atlas texture); phase B is the two mutable installs.
        // BUG-xaw4: bridge read leases taken below at each front read;
        // all retired on the final present encoder's completion (or a
        // marker buffer on the early returns).
        // Node previews change with every content frame, so the canvas draws
        // each frame while any shows (D17).
        let mut previews_visible = false;
        #[cfg(target_os = "macos")]
        {
            let atlas_handle =
                manifold_ui::node::texture_handle_for_key("__manifold_graph_node_atlas__");
            let atlas_tex = if let (Some(bridge), Some(_)) = (
                self.node_atlas_texture_bridge.as_ref(),
                self.graph_canvas.as_ref(),
            ) {
                let lease = bridge.acquire_read();
                let front = lease.slot();
                self.ui_node_atlas_textures
                    .get(front)
                    .and_then(|t| t.as_ref())
                    .map(|atlas_tex| {
                        bridge_leases.push((bridge.clone(), lease));
                        atlas_tex.clone()
                    })
            } else {
                None
            };
            if let Some(atlas_tex) = atlas_tex {
                if let Some(ui) = self.ui_renderer.as_mut() {
                    ui.register_external_texture(atlas_handle, atlas_tex);
                }
                if let Some(canvas) = self.graph_canvas.as_mut() {
                    let vp = crate::graph_canvas::Rect::new(canvas_x, 0.0, canvas_width, logical_h as f32);
                    // Each source is letterboxed into its 16:9 cell; the
                    // on-canvas screen takes the project aspect, so sample only
                    // the letterboxed content sub-rect. Edge-straddle clipping
                    // is the canvas viewport scissor, so no per-node UV
                    // cropping is needed here. The map refills in place and
                    // the atlas layout is a short list, so this allocates
                    // nothing per frame.
                    let layout = &self.content_state.node_atlas_layout;
                    previews_visible = canvas.refill_node_preview_src(vp, |id| {
                        let &(_, cell) = layout.iter().find(|(node, _)| node == id)?;
                        Some((atlas_handle, crate::content_pipeline::atlas_cell_uv(cell, monitor_aspect)))
                    });
                }
            }
        }

        // ── Composite the offscreen, only when something on it moved ──
        // Shared with the headless editor harness (`editor_frame.rs`). The
        // drawable still gets the offscreen and the monitors every pulse.
        let viewport_image = ws.viewport_session.as_ref()
            .and_then(|session| session.displayed.as_ref())
            .is_some_and(|frame| frame.has_image());
        let key = crate::editor_frame::CompositeKey {
            size: [logical_w, logical_h],
            dock: ws.dock.draw_key(),
            canvas: self.graph_canvas.as_ref().map_or(0, |canvas| canvas.view_version()),
            mini: mini_key,
            playhead: mini_current_beat.to_bits(),
            playing: mini_is_playing,
            live: crate::editor_frame::LiveSurfaces {
                node_previews: previews_visible,
                viewport_image,
                popover: self.editor_mapping_popover.is_open(),
                text_input: self.text_input.is_owned_by_editor(),
            },
        };
        let redraw = crate::editor_frame::needs_composite(&mut ws.editor_frame, &ws.ui_root, ws.offscreen_dirty, key);
        let cache = ws.editor_frame.cache.get_or_insert_with(|| {
            manifold_ui_paint::ui_cache_manager::UICacheManager::with_slots(
                manifold_compositor::presentation::UI_FORMAT,
                scale,
                manifold_ui_paint::ui_cache_manager::EDITOR_SLOTS,
            )
        });
        if redraw
            && let (Some(atlas_pipeline), Some(atlas_sampler)) = (&self.atlas_pipeline, &self.atlas_sampler)
        {
            crate::editor_frame::composite_editor_frame(
                &gpu.device,
                self.ui_renderer.as_mut(),
                &mut ws.ui_root,
                &layout,
                &ws.dock,
                self.graph_canvas.as_ref(),
                crate::editor_frame::EditorAtlas {
                    cache,
                    pipeline: atlas_pipeline,
                    sampler: atlas_sampler,
                    scale_factor: scale,
                },
                crate::editor_frame::EditorMiniTimelineInputs {
                    bottom_rect: layout.bottom,
                    show_bottom: ws.dock.show_bottom,
                    total_beats: mini_total_beats,
                    beats_per_bar: mini_beats_per_bar,
                    current_beat: mini_current_beat,
                    row_count: mini_rows,
                    clips: mini_clips,
                    layer_labels: mini_layer_labels,
                    readout: mini_readout,
                    is_playing: mini_is_playing,
                },
                &mut self.editor_mapping_popover,
                popover_live_value,
                &self.text_input,
                &self.frame_timer,
                offscreen,
                scale,
                ws.viewport_session.as_ref().and_then(|session| session.displayed.as_ref())
                    .filter(|frame| frame.has_image())
                    .zip(ws.viewport_rect)
                    .map(|(frame, rect)| crate::scene_viewport::SceneViewportPaint {
                        rect,
                        frame,
                        lines: &ws.viewport_overlay_lines,
                        #[cfg(target_os = "macos")]
                        // Gated on this frame's selection, recomputed above.
                        outline: self.selection_outline.as_ref().zip(viewport_mask.as_ref().filter(|_| !ws.viewport_outline.is_empty())),
                        #[cfg(not(target_os = "macos"))]
                        outline: None,
                    }),
            );
            ws.offscreen_dirty = false;
        }

        // Skip drawable acquisition on the resize frame — drawable pool
        // may still be reconfiguring.
        if ws.surface_resized_this_frame {
            ws.surface_resized_this_frame = false;
            #[cfg(target_os = "macos")]
            crate::shared_texture::retire_leases_via_marker(
                &gpu.device,
                self.content_tx.clone(),
                std::mem::take(&mut bridge_leases),
            );
            return;
        }

        // ── Late drawable acquisition + blit ──
        let Some(drawable) = surface.next_drawable() else {
            #[cfg(target_os = "macos")]
            crate::shared_texture::retire_leases_via_marker(
                &gpu.device,
                self.content_tx.clone(),
                std::mem::take(&mut bridge_leases),
            );
            return;
        };
        let drawable_tex = drawable.gpu_texture(manifold_compositor::presentation::UI_FORMAT);
        let (Some(blit_p), Some(blit_s)) = (&self.blit_pipeline, &self.blit_sampler) else {
            #[cfg(target_os = "macos")]
            crate::shared_texture::retire_leases_via_marker(
                &gpu.device,
                self.content_tx.clone(),
                std::mem::take(&mut bridge_leases),
            );
            return;
        };

        let mut present_enc = gpu.device.create_encoder("Graph Editor Present");
        present_enc.draw_fullscreen(
            blit_p,
            &drawable_tex,
            &[
                manifold_gpu::GpuBinding::Texture {
                    binding: 0,
                    texture: offscreen,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 1,
                    sampler: blit_s,
                },
            ],
            false,
            true,
            "Editor Offscreen → Drawable",
        );

        // ── Sidebar-top preview monitors ──
        // Composite the captured node texture (top) and the master compositor
        // output (below it) into the editor sidebar, each below its title.
        // Only when the IOSurface front buffer is available. The blit pipeline
        // targets the drawable's float presentation format, so these draw into the
        // drawable (Load) rather than the Rgba16Float offscreen.
        #[cfg(target_os = "macos")]
        {
            let scale = scale as f32;
            let w = preview_w * scale;
            let h = preview_h * scale;
            let x = preview_x * scale;
            // Node-output monitor — the selected image node's captured output.
            // Only when that node has an image; a non-image node's pane is
            // filled with the value inspector text instead (drawn above).
            // Suppressed while the P5c 3D viewport occupies this same pane
            // (below) — the two are mutually exclusive views of the same
            // reserved rect, never composited together.
            if show_image
                && ws.viewport_session.is_none()
                && let Some(bridge) = self.node_preview_texture_bridge.as_ref()
            {
                let lease = bridge.acquire_read();
                let front = lease.slot();
                if let Some(tex) = self
                    .ui_node_preview_textures
                    .get(front)
                    .and_then(|t| t.as_ref())
                {
                    bridge_leases.push((bridge.clone(), lease));
                    present_enc.draw_fullscreen_viewport(
                        blit_p,
                        &drawable_tex,
                        &[
                            manifold_gpu::GpuBinding::Texture {
                                binding: 0,
                                texture: tex,
                            },
                            manifold_gpu::GpuBinding::Sampler {
                                binding: 1,
                                sampler: blit_s,
                            },
                        ],
                        (x, node_img_y * scale, w, h),
                        manifold_gpu::GpuLoadAction::Load,
                        "Node Preview → Sidebar",
                    );
                }
            }
            // Master-out monitor — the live compositor output, the same texture
            // the main/perform window presents. Imported into `ui_preview_textures`
            // by the workspace-preview present path that runs just before this.
            if let Some(bridge) = self.preview_texture_bridge.as_ref() {
                let lease = bridge.acquire_read();
                let front = lease.slot();
                if let Some(tex) = self.ui_preview_textures.get(front).and_then(|t| t.as_ref()) {
                    bridge_leases.push((bridge.clone(), lease));
                    present_enc.draw_fullscreen_viewport(
                        blit_p,
                        &drawable_tex,
                        &[
                            manifold_gpu::GpuBinding::Texture {
                                binding: 0,
                                texture: tex,
                            },
                            manifold_gpu::GpuBinding::Sampler {
                                binding: 1,
                                sampler: blit_s,
                            },
                        ],
                        (x, master_img_y * scale, w, h),
                        manifold_gpu::GpuLoadAction::Load,
                        "Master Out → Sidebar",
                    );
                }
            }
        }

        present_enc.present_drawable(&drawable);
        // BUG-xaw4: every bridge lease taken for this frame retires when the
        // present encoder completes — it is ordered after the offscreen
        // passes that sampled the bridges (same queue).
        #[cfg(target_os = "macos")]
        crate::shared_texture::attach_lease_retire(
            &present_enc,
            self.content_tx.clone(),
            std::mem::take(&mut bridge_leases),
        );
        present_enc.commit();
    }
}

#[cfg(test)]
mod preview_target_tests {
    use super::{modifier_preview_selection, resolve_preview_target, stable_scope_path};
    use manifold_core::NodeId;
    use manifold_ui::graph_view::{
        GROUP_INPUT_TYPE_ID, GROUP_OUTPUT_TYPE_ID, GROUP_TYPE_ID, GraphSnapshot, GroupSnapshot,
        NodeSnapshot, PortKindSnapshot, PortSnapshot, WireSnapshot,
    };

    fn tex(name: &str) -> PortSnapshot {
        PortSnapshot {
            name: name.to_string(),
            kind: PortKindSnapshot::Texture2D,
        }
    }

    fn node(
        id: u32,
        node_id: &str,
        type_id: &str,
        ins: Vec<PortSnapshot>,
        outs: Vec<PortSnapshot>,
    ) -> NodeSnapshot {
        NodeSnapshot {
            id,
            node_id: NodeId::new(node_id),
            node_handle: None,
            type_id: type_id.to_string(),
            title: String::new(),
            inputs: ins,
            outputs: outs,
            parameters: Vec::new(),
            editor_pos: None,
            breaks_dependency_cycle: false,
            group: None,
            wgsl_source: None,
            category: manifold_ui::graph_view::Category::Uncategorized,
            tooltip: None,
        }
    }

    fn wire(fnode: u32, fport: &str, tnode: u32, tport: &str) -> WireSnapshot {
        WireSnapshot {
            from_node: fnode,
            from_port: fport.to_string(),
            to_node: tnode,
            to_port: tport.to_string(),
        }
    }

    fn snap(nodes: Vec<NodeSnapshot>, wires: Vec<WireSnapshot>) -> GraphSnapshot {
        GraphSnapshot {
            nodes,
            wires,
            outer_routings: Vec::new(),
        }
    }

    /// Group container (id 5, node_id "grp"): body is
    /// group_input(0) -> inner(1) -> group_output(2).
    fn group_container() -> NodeSnapshot {
        let mut g = node(5, "grp", GROUP_TYPE_ID, vec![tex("src")], vec![tex("out")]);
        g.group = Some(Box::new(GroupSnapshot {
            nodes: vec![
                node(0, "", GROUP_INPUT_TYPE_ID, vec![], vec![tex("src")]),
                node(1, "inner", "node.blur", vec![tex("in")], vec![tex("out")]),
                node(2, "", GROUP_OUTPUT_TYPE_ID, vec![tex("out")], vec![]),
            ],
            wires: vec![wire(0, "src", 1, "in"), wire(1, "out", 2, "out")],
            tint: None,
        }));
        g
    }

    #[test]
    fn plain_node_previews_itself() {
        let s = snap(
            vec![node(1, "blur", "node.blur", vec![tex("in")], vec![tex("out")])],
            vec![],
        );
        assert_eq!(resolve_preview_target(&s, &[], 1), Some(NodeId::new("blur")));
    }

    #[test]
    fn group_output_boundary_resolves_to_container() {
        let s = snap(
            vec![
                node(10, "src", "system.source", vec![], vec![tex("out")]),
                group_container(),
            ],
            vec![wire(10, "out", 5, "src")],
        );
        // Inside the group, the output boundary previews the group container,
        // so the content side maps it (+ port name) via group_preview_map.
        assert_eq!(resolve_preview_target(&s, &[5], 2), Some(NodeId::new("grp")));
    }

    #[test]
    fn group_input_boundary_resolves_to_external_feeder() {
        let s = snap(
            vec![
                node(10, "src", "system.source", vec![], vec![tex("out")]),
                group_container(),
            ],
            vec![wire(10, "out", 5, "src")],
        );
        // Inside the group, the input boundary previews whatever feeds the
        // group from outside (the source).
        assert_eq!(resolve_preview_target(&s, &[5], 0), Some(NodeId::new("src")));
    }

    #[test]
    fn final_output_resolves_to_its_input_producer() {
        let s = snap(
            vec![
                node(0, "src", "system.source", vec![], vec![tex("out")]),
                node(1, "inv", "node.invert", vec![tex("in")], vec![tex("out")]),
                node(2, "", "system.final_output", vec![tex("in")], vec![]),
            ],
            vec![wire(0, "out", 1, "in"), wire(1, "out", 2, "in")],
        );
        assert_eq!(resolve_preview_target(&s, &[], 2), Some(NodeId::new("inv")));
    }

    #[test]
    fn modifier_preview_scope_uses_stable_groups_and_resolved_boundary_scope() {
        let s = snap(
            vec![
                node(10, "source", "system.source", vec![], vec![tex("out")]),
                group_container(),
            ],
            vec![wire(10, "out", 5, "src")],
        );
        assert_eq!(stable_scope_path(&s, &[5]), Some(vec![NodeId::new("grp")]));
        // The group-output boundary resolves to the group container, which
        // lives at the parent scope rather than inside the canvas scope.
        let resolved = resolve_preview_target(&s, &[5], 2).unwrap();
        assert_eq!(resolved, NodeId::new("grp"));
        assert_eq!(modifier_preview_selection(&s, &[5], Some(2)), (Some(resolved), Vec::<NodeId>::new()));
        // A normal inner node retains the enclosing stable group path.
        assert_eq!(
            modifier_preview_selection(&s, &[5], Some(1)),
            (Some(NodeId::new("inner")), vec![NodeId::new("grp")])
        );

        let mut sibling = group_container();
        sibling.id = 6;
        sibling.node_id = NodeId::new("sibling");
        let siblings = snap(vec![group_container(), sibling], vec![]);
        assert_eq!(
            modifier_preview_selection(&siblings, &[6], Some(1)),
            (Some(NodeId::new("inner")), vec![NodeId::new("sibling")])
        );
    }
}

/// `PARAM_TWO_WAY_BINDING_DESIGN.md` P1: the dispatch-layer binding lookup
/// (`binding_for_node_param`) and wired-backstop (`node_param_is_wired`) that
/// the `SetGraphNodeParam` reroute arm depends on. These are the read-only
/// resolution pieces the reroute is built from; `card_reshape_roundtrips` /
/// `macro_curve_inverse_roundtrips` (manifold-core) cover the inverse math
/// itself. A full `Application`-level drive of the dispatch arm (mutating
/// `local_project`, asserting the def slot stays byte-unchanged) is not
/// exercised here — `Application` needs a winit/GPU context this test module
/// has no harness for; see the phase notes in
/// `docs/PARAM_TWO_WAY_BINDING_DESIGN.md` for the gap.
#[cfg(test)]
mod binding_reroute_tests {
    use super::{binding_for_node_param, node_param_is_wired};
    use crate::app::Application;
    use crate::content_command::ContentCommand;
    use manifold_core::NodeId;
    use manifold_core::effect_graph_def::{
        BindingDef, BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire, ParamSpecDef,
        PresetMetadata,
    };
    use manifold_core::macro_bank::MacroCurve;
    use manifold_core::scene_modifier_preset::{
        SceneModifierInstanceDef, SceneNodeRef, SceneTargetSelection,
    };
    use std::collections::{BTreeMap, BTreeSet};

    fn node(id: u32, node_id: &str) -> EffectGraphNode {
        EffectGraphNode {
            id,
            node_id: NodeId::new(node_id),
            type_id: "node.blur".to_string(),
            handle: Some(node_id.to_string()),
            params: BTreeMap::new(),
            exposed_params: BTreeSet::new(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        }
    }

    fn def_with_binding() -> EffectGraphDef {
        EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: Some(PresetMetadata {
                shape: None,
                id: manifold_core::preset_type_id::PresetTypeId::new("Test"),
                display_name: "Test".into(),
                category: "Test".into(),
                osc_prefix: "test".into(),
                legacy_discriminant: None,
                scene_modifier: None,
                scene_bounds: None,
                available: true,
                is_line_based: false,
                    layer_types: None,
                params: vec![ParamSpecDef {
                    tooltip: None,
                    id: "amount".into(),
                    name: "Amount".into(),
                    min: 0.0,
                    max: 1.0,
                    default_value: 0.0,
                    whole_numbers: false,
                    is_toggle: false,
                    is_trigger: false,
                    value_labels: vec![],
                    format_string: None,
                    osc_suffix: String::new(),
                    curve: MacroCurve::SCurve,
                    invert: true,
                    is_angle: false,
                    is_trigger_gate: false,
                    wraps: false,
                    section: None,
                    card_visible: true,
            material_role: None,
                    limit: Default::default(),
                    unit: None,
                    scene: None,
                }],
                bindings: vec![BindingDef {
                    id: "amount".into(),
                    label: "Amount".into(),
                    default_value: 0.0,
                    target: BindingTarget::Node {
                        node_id: NodeId::new("blur1"),
                        param: "amount".into(),
                    },
                    convert: Default::default(),
                    user_added: false,
                    scale: 2.0,
                    offset: 0.5,
                    default_mirrors_node_param: false,
                }],
                param_aliases: vec![],
                value_aliases: vec![],
                string_params: vec![],
                string_bindings: vec![],
            }),
            scene_modifiers: Vec::new(),
            nodes: vec![node(1, "blur1")],
            wires: vec![],
        }
    }

    fn manifest_for(def: &EffectGraphDef) -> manifold_core::params::ParamManifest {
        let mut params = manifold_core::params::ParamManifest::default();
        for spec in &def.preset_metadata.as_ref().unwrap().params {
            params.push(manifold_core::params::Param::bundled(spec.clone()));
        }
        params
    }

    fn mapping_test_app() -> (Application, manifold_core::GraphTarget, manifold_core::GraphTarget) {
        use manifold_core::{effects::PresetInstance, GraphTarget, PresetTypeId};
        let mut app = Application::new();
        app.user_prefs = crate::user_prefs::UserPrefs::for_test();
        let def = def_with_binding();
        let mut effect = PresetInstance::new(PresetTypeId::new("Test"));
        effect.params = manifest_for(&def);
        effect.graph = Some(def.clone());
        let a = GraphTarget::Effect(effect.id.clone());
        app.local_project.intake().settings.master_effects.push(effect);
        let mut layer = manifold_core::layer::Layer::new_generator("Test".into(), PresetTypeId::new("Test"), 0);
        let b = GraphTarget::Generator(layer.layer_id.clone());
        let generator = layer.gen_params_mut().unwrap();
        generator.params = manifest_for(&def);
        generator.graph = Some(def);
        app.local_project.intake().timeline.layers.push(layer);
        app.watched_graph_target = Some(a.clone());
        (app, a, b)
    }

    #[test]
    fn editor_card_lane_shows_live_values_without_rebuilding() {
        use manifold_ui::panels::Panel;
        // Effect cards require a registered preset definition. Keep the real
        // Bloom identity and add a parameter, as the projection tests do.
        let mut project = manifold_core::project::Project::default();
        let mut effect = manifold_core::effects::PresetInstance::new(manifold_core::PresetTypeId::BLOOM);
        let spec = def_with_binding().preset_metadata.unwrap().params.remove(0);
        effect.params = manifold_core::params::ParamManifest::from_params(vec![
            manifold_core::params::Param::user_added(spec),
        ]);
        project.settings.master_effects.push(effect);
        let mut ui = crate::ui_root::UIRoot::new();
        crate::ui_bridge::sync_inspector_data(
            &mut ui, &project, None,
            &crate::app::SelectionState::default(), &[], None,
        );
        let mut state = crate::editor_frame::EditorFrameState::default();
        let panel = manifold_ui::panels::graph_editor::GraphEditorPanel::new();
        let layout = crate::editor_frame::EditorLayout::compute(&manifold_ui::Dock::editor(), 1600, 2000, None);
        let content_state = crate::content_state::ContentState::default();
        let mut live = crate::frame_plan::LiveInputs::default();
        let mut run = |ui: &mut crate::ui_root::UIRoot, project: &manifold_core::project::Project, live| {
            let start = manifold_core::work_count::thread_work_snapshot();
            crate::editor_frame::run_editor_frame(
                ui,
                &mut state,
                &panel,
                None,
                &layout,
                crate::editor_frame::EditorFrameInputs {
                    project,
                    content_state: &content_state,
                    active_layer: None,
                    live,
                    dt: 1.0 / 60.0,
                    show_image: false,
                },
                false,
            );
            manifold_core::work_count::thread_work_snapshot()
                .since(&start)
                .get(manifold_core::work_count::WorkKind::EditorTreeBuild)
        };
        // The card lane is built on the first frame; once its spawn tweens
        // settle, values reach it through the live push alone. A repeated
        // value is an unchanged frame.
        let mut last = None;
        for (frame, (value, expected)) in [(0.0, "0.00"), (0.3, "0.30"), (0.3, "0.30"), (0.8, "0.80")].into_iter().enumerate() {
            project.settings.master_effects[0]
                .params.get_mut("amount").unwrap().value = value;
            if last != Some(value) {
                live.values += 1;
            }
            last = Some(value);
            let built_before = ui.inspector.first_node();
            let rebuilt = run(&mut ui, &project, live);
            assert_eq!(rebuilt > 0, frame == 0);
            assert!(frame == 0 || built_before == ui.inspector.first_node());
            assert!(ui.tree.all_nodes().any(|node| node.text.as_deref() == Some(expected)),
                "the card lane must show live value {expected}");
            if frame == 0 {
                ui.inspector.skip_to_settled(&mut ui.tree);
                run(&mut ui, &project, live);
            }
        }
    }

    #[test]
    fn inspector_numeric_entry_owns_emitting_window_and_commits_one_undo() {
        use manifold_ui::{GraphParamTarget, PanelAction, RootAction, Rect};
        use crate::text_input::TextInputField;
        use manifold_editing::undo::UndoRedoManager;
        for in_editor in [false, true] {
            let (mut app, _, owner) = mapping_test_app();
            let manifold_core::GraphTarget::Generator(layer) = &owner else { unreachable!() };
            let action = PanelAction::Root(RootAction::BeginParamTextInput {
                target: GraphParamTarget::GeneratorOf(layer.clone()), param_id: "amount".into(),
                anchor: Rect::new(100.0, 300.0, 56.0, 24.0), value: 0.0,
                min: 0.0, max: 1.0, whole_numbers: false, degrees: false,
            });
            let mut project = app.local_project.clone();
            let (tx, rx) = crossbeam_channel::unbounded();
            app.content_tx = Some(tx);
            assert!(app.dispatch_inspector_host_action(&action, in_editor));
            assert_eq!(app.text_input.is_owned_by_editor(), in_editor);
            assert_eq!(app.text_input.is_owned_by_main(), !in_editor);
            assert_eq!(app.text_input.field, if in_editor {
                TextInputField::EditorInspectorParam
            } else { TextInputField::InspectorParam });
            app.text_input.paste("0.8");
            let (field, text) = app.text_input.commit();
            app.handle_text_input_commit(field, &text);
            let mut undo = UndoRedoManager::new();
            for command in rx.try_iter() {
                match command {
                    ContentCommand::MutateProject { f: edit, .. } => edit(&mut project),
                    ContentCommand::Execute(command) => { undo.execute(command, &mut project); }
                    // Typed entry runs the slider gesture, so it holds and releases the
                    // param for armed recording; holds never touch the project.
                    ContentCommand::AutomationHold { .. } => {}
                    _ => panic!("unexpected numeric-entry command"),
                }
            }
            let value = |p: &manifold_core::project::Project| {
                p.preset_instance(&owner).unwrap().params.get("amount").unwrap().base
            };
            assert_eq!(value(&project), 0.8);
            assert_eq!(undo.undo_count(), 1);
            assert!(undo.undo(&mut project));
            assert_eq!(value(&project), 0.0);
            assert!(undo.redo(&mut project));
            assert_eq!(value(&project), 0.8);
        }
    }

    fn scene_mapping_test_project() -> (manifold_core::project::Project, manifold_core::GraphTarget) {
        use manifold_core::{layer::Layer, GraphTarget, PresetTypeId};
        let local = def_with_binding();
        let mut host = def_with_binding();
        let metadata = host.preset_metadata.as_mut().unwrap();
        metadata.params[0].id = "macro".into();
        metadata.params[0].name = "Macro".into();
        metadata.bindings[0] = BindingDef {
            id: "macro".into(),
            label: "Macro".into(),
            default_value: 0.25,
            target: BindingTarget::SceneModifier {
                modifier_id: NodeId::new("modifier"),
                param_id: "amount".into(),
            },
            convert: Default::default(),
            user_added: false,
            scale: 3.0,
            offset: 4.0,
            default_mirrors_node_param: false,
        };
        host.scene_modifiers.push(SceneModifierInstanceDef {
            id: NodeId::new("modifier"),
            scene: SceneNodeRef { scope: vec![], node: NodeId::new("scene") },
            targets: SceneTargetSelection::AllObjects,
            mesh_frames: Vec::new(),
            legacy_math_view_carrier: None,
            graph: Box::new(local),
        });
        let mut layer = Layer::new_generator("Generator".into(), PresetTypeId::new("Test"), 0);
        layer.layer_id = manifold_core::LayerId::new("layer");
        let target = GraphTarget::SceneModifier {
            owner: Box::new(GraphTarget::Generator(layer.layer_id.clone())),
            modifier_id: NodeId::new("modifier"),
        };
        let generator = layer.gen_params_mut().unwrap();
        generator.params = manifest_for(&host);
        generator.graph = Some(host);
        let mut project = manifold_core::project::Project::default();
        project.timeline.layers.push(layer);
        (project, target)
    }

    #[test]
    fn mapping_first_open_uses_card_owner_and_clicked_anchor() {
        use manifold_ui::{GraphParamTarget, node::Rect};
        let (mut app, previous, owner) = mapping_test_app();
        let manifold_core::GraphTarget::Generator(layer) = &owner else { unreachable!() };
        let mut editor = crate::workspace::Workspace::new(crate::workspace::WorkspaceKind::GraphEditor);
        let region = editor.ui_root.tree.begin_region(Rect::new(0.0, 0.0, 1000.0, 900.0),
            manifold_ui::tree::ZTier::Base, "mapping-test", manifold_ui::UIFlags::empty());
        let start = editor.ui_root.tree.count();
        let upper = editor.ui_root.tree.add_node(None, Rect::new(50.0, 30.0, 16.0, 18.0),
            manifold_ui::node::UINodeType::Button, Default::default(), None, manifold_ui::UIFlags::empty());
        let lower = editor.ui_root.tree.add_node(None, Rect::new(50.0, 330.0, 16.0, 18.0),
            manifold_ui::node::UINodeType::Button, Default::default(), None, manifold_ui::UIFlags::empty());
        assert_ne!(upper, lower);
        editor.ui_root.tree.end_region(region, start);
        app.graph_editor = Some(editor);
        assert!(app.open_card_mapping(&GraphParamTarget::GeneratorOf(layer.clone()), "amount", lower,
            manifold_ui::graph_canvas::Rect::new(0.0, 0.0, 1000.0, 900.0)));
        assert_eq!(app.editor_mapping_popover.target(), Some(&crate::editing_host::to_ui_graph_target(&owner)));
        assert_eq!(app.watched_graph_target.as_ref(), Some(&owner));
        // A press beside the lower row reaches the modal; the upper-row
        // position must not. Same parameter name cannot choose its anchor.
        assert!(!app.editor_mapping_popover.on_press(80.0, 40.0));
        assert!(app.editor_mapping_popover.on_press(80.0, 340.0));
        let manifold_core::GraphTarget::Effect(id) = previous else { unreachable!() };
        app.watch_effect_graph(id);
        assert!(!app.editor_mapping_popover.is_open(), "changing graphs dismisses the old modal");
    }

    #[test]
    fn mapping_gesture_survives_selection_change_and_commits_one_undo() {
        use manifold_ui::RootAction;
        use manifold_editing::undo::UndoRedoManager;
        let (mut app, owner, other) = mapping_test_app();
        let (tx, rx) = crossbeam_channel::unbounded();
        app.content_tx = Some(tx);
        let mut content_project = app.local_project.clone();
        let target = crate::editing_host::to_ui_graph_target(&owner);
        app.dispatch_mapping_action(&RootAction::EffectMappingRangeSnapshot { target: target.clone(), binding_id: "amount".into() });
        app.watched_graph_target = Some(other.clone());
        for max in [2.0, 4.0, 7.0] {
            app.dispatch_mapping_action(&RootAction::EffectMappingRangeChanged {
                target: target.clone(), binding_id: "amount".into(), min: 0.0, max,
            });
        }
        // A stale terminal event for a different card must not steal this drag.
        app.dispatch_mapping_action(&RootAction::EffectMappingRangeCommit {
            target: crate::editing_host::to_ui_graph_target(&other), binding_id: "amount".into(),
        });
        assert!(app.scrub.active.is_some());
        assert_eq!(super::target_full_reshape(&app.local_project, &owner, "amount").unwrap().2, 7.0);
        assert_eq!(super::target_full_reshape(&app.local_project, &other, "amount").unwrap().2, 1.0);
        app.dispatch_mapping_action(&RootAction::EffectMappingRangeCommit { target, binding_id: "amount".into() });
        assert!(app.scrub.active.is_none());
        let mut undo = UndoRedoManager::new();
        for command in rx.try_iter() {
            match command {
                ContentCommand::MutateProject { f: edit, .. } => edit(&mut content_project),
                ContentCommand::Execute(command) => {
                    undo.execute(command, &mut content_project);
                }
                _ => panic!("unexpected mapping command"),
            }
        }
        assert_eq!(undo.undo_count(), 1);
        assert_eq!(content_project.preset_instance(&owner).unwrap().params.get("amount").unwrap().slider_span().1, 7.0);
        assert!(undo.undo(&mut content_project));
        assert_eq!(content_project.preset_instance(&owner).unwrap().params.get("amount").unwrap().slider_span().1, 1.0);
        assert!(undo.redo(&mut content_project));
        assert_eq!(content_project.preset_instance(&owner).unwrap().params.get("amount").unwrap().slider_span().1, 7.0);
    }

    #[test]
    fn mapping_cancel_restores_captured_owner_without_undo() {
        use manifold_ui::RootAction;
        let (mut app, owner, other) = mapping_test_app();
        let (tx, rx) = crossbeam_channel::unbounded();
        app.content_tx = Some(tx);
        let target = crate::editing_host::to_ui_graph_target(&owner);
        app.dispatch_mapping_action(&RootAction::EffectMappingAffineSnapshot { target: target.clone(), binding_id: "amount".into() });
        app.watched_graph_target = Some(other.clone());
        app.dispatch_mapping_action(&RootAction::EffectMappingAffineChanged { target: target.clone(), binding_id: "amount".into(), scale: 9.0, offset: 4.0 });
        assert_eq!(super::target_full_reshape(&app.local_project, &owner, "amount").unwrap().5, 9.0);
        app.dispatch_mapping_action(&RootAction::EffectMappingCancel { target, binding_id: "amount".into() });
        assert!(app.scrub.active.is_none());
        assert_eq!(super::target_full_reshape(&app.local_project, &owner, "amount").unwrap().5, 2.0);
        assert_eq!(super::target_full_reshape(&app.local_project, &other, "amount").unwrap().5, 2.0);
        assert!(rx.try_iter().all(|cmd| matches!(cmd, ContentCommand::MutateProject { touches, .. } if touches.minus(crate::content_command::touch::OVERRIDES_SHAPES).is_empty())));
    }

    #[test]
    fn scene_modifier_mapping_composes_and_writes_host_macro() {
        use manifold_editing::commands::effects::BindingMappingEdit;
        let (project, target) = scene_mapping_test_project();
        let composed = super::modifier_binding_for_node_param(&project, &target, &[], 1, "amount")
            .expect("local binding resolves through outer macro");
        assert_eq!(composed.0, "macro");
        assert_eq!(composed.5, 6.0);
        assert_eq!(composed.6, 8.5);

        let mut app = Application::new();
        app.local_project = crate::ui_bridge::LocalProject::new(project.clone());
        // The mapping surface edits the public macro affine once; node-face
        // inversion above alone uses the composed leaf affine.
        assert_eq!(super::target_full_reshape(&app.local_project, &target, "macro").unwrap().5, 3.0);
        let (tx, rx) = crossbeam_channel::unbounded();
        app.content_tx = Some(tx);
        app.mapping_host().unwrap().commit(&target, "macro", BindingMappingEdit {
            max: Some(7.0), scale: Some(4.0), ..Default::default()
        });
        let mut content_project = project;
        let command = rx
            .try_iter()
            .find_map(|cmd| match cmd {
                ContentCommand::Execute(command) => Some(command),
                _ => None,
            })
            .expect("valid local mapping queues a host command");
        let mut undo = manifold_editing::undo::UndoRedoManager::new();
        undo.execute(command, &mut content_project);
        assert_eq!(content_project.graph_target_owner(&target).unwrap().params.get("macro").unwrap().slider_span().1, 7.0);
        assert_eq!(super::modifier_binding_for_node_param(&content_project, &target, &[], 1, "amount").unwrap().5, 8.0);
        assert!(undo.undo(&mut content_project));
        assert_eq!(content_project.graph_target_owner(&target).unwrap().params.get("macro").unwrap().slider_span().1, 1.0);
    }

    #[test]
    fn scene_modifier_mapping_rejects_missing_local_binding_without_queueing() {
        use manifold_editing::commands::effects::BindingMappingEdit;
        let (mut project, target) = scene_mapping_test_project();
        project
            .graph_target_owner_mut(&target)
            .unwrap()
            .graph
            .as_mut()
            .unwrap()
            .scene_modifiers[0]
            .graph
            .preset_metadata
            .as_mut()
            .unwrap()
            .bindings
            .clear();
        let mut app = Application::new();
        app.local_project = crate::ui_bridge::LocalProject::new(project);
        let (tx, rx) = crossbeam_channel::unbounded();
        app.content_tx = Some(tx);
        app.mapping_host().unwrap().commit(&target, "macro", BindingMappingEdit {
            max: Some(9.0), ..Default::default()
        });
        assert!(rx.try_iter().next().is_none(), "invalid local mapping must be atomic");
    }

    #[test]
    fn binding_for_node_param_resolves_and_inverts() {
        let def = def_with_binding();
        let (outer_id, min, max, invert, curve, scale, offset) =
            binding_for_node_param(&manifest_for(&def), &def, &[], 1, "amount").expect("binding resolves");
        assert_eq!(outer_id, "amount");
        let target =
            manifold_core::effects::apply_card_reshape(0.5, min, max, invert, curve, scale, offset);
        let back =
            manifold_core::effects::invert_card_reshape(target, min, max, invert, curve, scale, offset)
                .expect("non-degenerate scale");
        assert!((back - 0.5).abs() < 1e-3, "expected ~0.5, got {back}");
    }

    // Regression probes retained from the initial audit.
    #[test]
    fn mapping_reader_follows_live_manifest() {
        use manifold_core::{effects::PresetInstance, params::Param, GraphTarget};
        use manifold_editing::command::Command;
        use manifold_editing::commands::effects::{BindingMappingEdit, EditParamMappingCommand};
        let def = def_with_binding();
        let mut fx = PresetInstance::new(manifold_core::PresetTypeId::new("Audit"));
        fx.params.push(Param::bundled(def.preset_metadata.as_ref().unwrap().params[0].clone()));
        fx.graph = Some(def);
        let target = GraphTarget::Effect(fx.id.clone());
        let mut project = manifold_core::project::Project::default();
        project.settings.master_effects.push(fx);
        let before = super::full_reshape_from_instance(&project.settings.master_effects[0], project.settings.master_effects[0].graph.as_ref().unwrap(), "amount").unwrap();
        EditParamMappingCommand::new(target, "amount".into(), BindingMappingEdit {
            max: Some(7.0), ..Default::default()
        }, None).execute(&mut project);
        let fx = &project.settings.master_effects[0];
        assert_eq!(fx.params.get("amount").unwrap().slider_span().1, 7.0, "production edit landed");
        let after = super::full_reshape_from_instance(fx, fx.graph.as_ref().unwrap(), "amount").unwrap();
        eprintln!("AUDIT: before max={}, live manifest max=7, editor reader max={}; range commit detects change={}", before.2, after.2, (before.2-after.2).abs() > f32::EPSILON);
        assert_eq!(after.2, 7.0, "mapping opener and commit reader must see the live edit");
        let inverse = binding_for_node_param(&fx.params, fx.graph.as_ref().unwrap(), &[], 1, "amount").unwrap();
        assert_eq!(inverse.2, 7.0, "node-face inverse mapping must use the same live range");
    }

    #[test]
    fn mapping_canvas_supports_generator_and_stock_bindings() {
        use manifold_core::{effects::PresetInstance, params::Param, GraphTarget};
        use manifold_node_engine::snapshot::{GraphSnapshot, NodeSnapshot};
        let mut def = def_with_binding();
        def.preset_metadata.as_mut().unwrap().bindings[0].user_added = true;
        let mut fx = PresetInstance::new(manifold_core::PresetTypeId::new("Audit"));
        fx.params.push(Param::bundled(def.preset_metadata.as_ref().unwrap().params[0].clone()));
        fx.graph = Some(def.clone());
        let effect_target = GraphTarget::Effect(fx.id.clone());
        let mut project = manifold_core::project::Project::default();
        project.settings.master_effects.push(fx);
        let mut layer = manifold_core::layer::Layer::new_generator("Audit".into(), manifold_core::PresetTypeId::new("Audit"), 0);
        let generator_target = GraphTarget::Generator(layer.layer_id.clone());
        let gp = layer.gen_params_mut().unwrap();
        gp.params.push(Param::bundled(def.preset_metadata.as_ref().unwrap().params[0].clone()));
        gp.graph = Some(def);
        project.timeline.layers.push(layer);
        let snapshot = GraphSnapshot { nodes: vec![NodeSnapshot {
            id: 1, node_id: NodeId::new("blur1"), node_handle: Some("blur1".into()),
            type_id: "node.blur".into(), title: "Blur".into(), inputs: vec![], outputs: vec![],
            parameters: vec![], editor_pos: None, breaks_dependency_cycle: false,
            group: None, wgsl_source: None,
        }], wires: vec![], outer_routings: vec![] };
        let effect_user = super::resolve_canvas_binding(Some(&snapshot), Some(&effect_target), &project, 1, "amount").is_some();
        assert!(effect_user, "positive control: effect user binding resolves");
        let generator_user = super::resolve_canvas_binding(Some(&snapshot), Some(&generator_target), &project, 1, "amount").is_some();
        project.settings.master_effects[0].graph.as_mut().unwrap().preset_metadata.as_mut().unwrap().bindings[0].user_added = false;
        let effect_stock = super::resolve_canvas_binding(Some(&snapshot), Some(&effect_target), &project, 1, "amount").is_some();
        eprintln!("AUDIT: effect user={effect_user}, generator user={generator_user}, effect stock={effect_stock}");
        assert!(generator_user && effect_stock, "equivalent bound rows should expose the same mapping workflow");
    }

    #[test]
    fn binding_for_node_param_none_when_unbound() {
        let def = def_with_binding();
        assert!(binding_for_node_param(&manifest_for(&def), &def, &[], 1, "other_param").is_none());
        assert!(binding_for_node_param(&manifest_for(&def), &def, &[], 99, "amount").is_none());
    }

    #[test]
    fn node_param_is_wired_detects_incoming_wire() {
        let mut def = def_with_binding();
        def.wires.push(EffectGraphWire {
            from_node: 0,
            from_port: "out".into(),
            to_node: 1,
            to_port: "amount".into(),
        });
        assert!(node_param_is_wired(&def, &[], 1, "amount"));
        assert!(!node_param_is_wired(&def, &[], 1, "other_param"));
    }
}

/// The renderer snapshot's UI translation, cached by `Arc` identity.
pub(crate) type EditorUiGraph = Option<(
    std::sync::Arc<manifold_node_engine::snapshot::GraphSnapshot>,
    std::sync::Arc<manifold_ui::graph_view::GraphSnapshot>,
)>;

/// The watched graph as the UI-local view-model the canvas reads,
/// translating the content state's renderer snapshot once per new snapshot.
/// `None` while the snapshot belongs to another target (a retarget the
/// content side has not answered yet) or nothing is watched.
pub(crate) fn editor_ui_graph(
    cache: &mut EditorUiGraph,
    content_state: &crate::content_state::ContentState,
    watched: Option<&manifold_core::GraphTarget>,
    project: &manifold_core::project::Project,
) -> Option<std::sync::Arc<manifold_ui::graph_view::GraphSnapshot>> {
    if content_state.active_graph_target.as_deref() != watched {
        return None;
    }
    let src = content_state.active_graph_snapshot.as_ref()?;
    let fresh = matches!(cache, Some((cached, _)) if std::sync::Arc::ptr_eq(cached, src));
    if !fresh {
        let mut ui = crate::ui_translate::graph_snapshot_to_ui(src);
        if let Some(target @ manifold_core::GraphTarget::SceneModifier { .. }) = watched
            && let Some(local) = crate::graph_target::resolve(project, target)
        {
            annotate_preparation_controls(&mut ui.nodes, &local);
        }
        *cache = Some((src.clone(), std::sync::Arc::new(ui)));
    }
    cache.as_ref().map(|(_, ui)| ui.clone())
}

/// Preparation controls keep the shared numeric editor, with an explicit
/// setup label and no exposure or mapping affordance.
fn annotate_preparation_controls(
    nodes: &mut [manifold_ui::graph_view::NodeSnapshot],
    local: &manifold_core::effect_graph_def::EffectGraphDef,
) {
    let Some(metadata) = local.preset_metadata.as_ref() else { return; };
    let Some(recipe) = metadata.scene_modifier.as_ref() else { return; };
    for node in nodes {
        for param in &mut node.parameters {
            let preparation = metadata.bindings.iter().any(|binding|
                recipe.preparation_params.contains(&binding.id)
                    && matches!(&binding.target, manifold_core::effect_graph_def::BindingTarget::Node { node_id, param: name }
                        if node_id == &node.node_id && name == &param.name));
            if preparation {
                param.preparation_only = true;
                param.exposed = false;
                param.label.push_str(" · Setup");
                param.tooltip = Some("Applies when released and rebuilds the scene. This control cannot be modulated; editing it stores a fixed default.".into());
            }
        }
        if let Some(group) = &mut node.group {
            annotate_preparation_controls(&mut group.nodes, local);
        }
    }
}
