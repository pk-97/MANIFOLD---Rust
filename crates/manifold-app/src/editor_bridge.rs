//! Graph-editor bridge: the watched-graph reads, param-mapping preview/commit,
//! preset-save/rename prompts, node copy, the bound/unbound node-param drag
//! sessions, and the graph-editor window present. Moved verbatim from
//! app_render.rs (UI_FUNNEL_DECOMPOSITION P-F1, pure move).

use crate::app::Application;
use crate::content_command::ContentCommand;
use manifold_editing::commands::effects::BindingMappingEdit;
use crate::app_render::mini_timeline_data;

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
    match target {
        manifold_core::GraphTarget::Effect(eid) => {
            let fx = project.find_effect_by_id(eid)?;
            if fx.graph.is_some() {
                return None;
            }
            let view = manifold_renderer::node_graph::loaded_preset_view_by_id(fx.effect_type())?;
            Some((*view.canonical_def).clone())
        }
        manifold_core::GraphTarget::Generator(lid) => {
            let layer = project
                .timeline
                .layers
                .iter()
                .find(|l| &l.layer_id == lid)?;
            if layer.generator_graph().is_some() {
                return None;
            }
            let gp = layer.gen_params()?;
            manifold_renderer::node_graph::loaded_preset_view_by_id(gp.generator_type())
                .map(|v| (*v.canonical_def).clone())
        }
    }
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

/// The current reshape for `param_id` read out of a preset graph def:
/// `(label, min, max, invert, curve, scale, offset)`. Range/curve/invert/label
/// live on the param's [`ParamSpecDef`]; scale/offset on its [`BindingDef`]
/// (identity `1.0`/`0.0` when the param has no binding). `None` when the def
/// carries no metadata or the param id isn't found.
fn full_reshape_from_def(
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
    let spec = meta.params.iter().find(|p| p.id == param_id)?;
    let (scale, offset) = meta
        .bindings
        .iter()
        .find(|b| b.id == param_id)
        .map(|b| (b.scale, b.offset))
        .unwrap_or((1.0, 0.0));
    Some((
        spec.name.clone(),
        spec.min,
        spec.max,
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
/// here). Mirrors [`full_reshape_from_def`]'s reshape lookup, keyed the
/// other way (by node target instead of outer id). `None` when the node has
/// no stable id (a bundled node that's never been targeted), the level
/// doesn't resolve, or no binding targets this param.
fn binding_for_node_param(
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
        BindingTarget::Composite { .. } => false,
    })?;
    let spec = meta.params.iter().find(|p| p.id == binding.id)?;
    Some((
        binding.id.clone(),
        spec.min,
        spec.max,
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
fn node_param_value(
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

/// Convert a renderer-side [`manifold_renderer::node_graph::NodeSnapshot`]
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
fn find_snapshot_node<'a>(
    nodes: &'a [manifold_renderer::node_graph::NodeSnapshot],
    id: &manifold_core::NodeId,
) -> Option<&'a manifold_renderer::node_graph::NodeSnapshot> {
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

/// Resolve an on-canvas param row `(node_id, inner_param)` to the
/// matching card `UserParamBinding` on the watched effect, returning the
/// data the mapping popover needs to open. `None` when there's no active
/// snapshot/target, the node has no stable handle, or the inner param
/// isn't exposed as a user binding (only user-bound rows get the popover;
/// preset/static routings and plain inner params don't).
///
/// Returned tuple: `(binding_id, label, min, max, invert, curve, range)`.
/// `range` is the binding's declared inner-param bounds, used to span the
/// popover's trim track.
///
/// Free function (not a method) so the editor-window mouse handler can
/// call it while the `&mut GraphCanvas` borrow is live: it takes the
/// disjoint `self` fields (snapshot, target, project) by reference rather
/// than borrowing all of `self`.
#[allow(clippy::type_complexity)]
pub(crate) fn resolve_canvas_binding(
    snapshot: Option<&manifold_renderer::node_graph::GraphSnapshot>,
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
    // Only effect graphs carry card user-bindings; a generator target has none.
    let manifold_core::GraphTarget::Effect(eid) = target? else {
        return None;
    };
    let fx = project.find_effect_by_id(eid)?;
    let b = fx
        .user_param_bindings()
        .into_iter()
        .find(|b| b.node_id == node.node_id && b.inner_param == inner_param)?;
    Some((
        b.id.clone(),
        b.label.clone(),
        b.min,
        b.max,
        b.invert,
        b.curve,
        b.scale,
        b.offset,
        range,
        b.section.clone(),
    ))
}

impl Application {
    /// The mapping drawer's store target for the editor's watched graph —
    /// the [`manifold_core::GraphTarget`] the command then resolves to a
    /// `GraphHost`.
    pub(crate) fn mapping_target(&self) -> Option<manifold_core::GraphTarget> {
        self.watched_graph_target.clone()
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

    /// Read the watched param's CURRENT reshape `(min, max, scale, offset)`
    /// for the drawer seed + drag change-detection. Reads the preset's
    /// authoring surface (`ParamSpecDef` range + `BindingDef` scale/offset)
    /// from whichever graph def is live: the instance's per-instance override
    /// if it has diverged, else the catalog graph (effect) / bundled def
    /// (generator). `None` if the param doesn't resolve. This is the single
    /// reshape source after `ParamMapping` was deleted.
    pub(crate) fn watched_reshape(&self, param_id: &str) -> Option<(f32, f32, f32, f32)> {
        let (_, min, max, _, _, scale, offset) = self.watched_full_reshape(param_id)?;
        Some((min, max, scale, offset))
    }

    /// The watched param's full reshape `(label, min, max, invert, curve,
    /// scale, offset)` — the drawer's complete seed. Reads the instance's
    /// per-instance graph override first, then falls back to the catalog
    /// (effect) / bundled (generator) graph def.
    pub(crate) fn watched_full_reshape(
        &self,
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
        match self.watched_graph_target.as_ref()? {
            manifold_core::GraphTarget::Effect(eid) => {
                let fx = self.local_project.find_effect_by_id(eid)?;
                if let Some(def) = fx.graph.as_ref()
                    && let Some(r) = full_reshape_from_def(def, param_id)
                {
                    return Some(r);
                }
                let view =
                    manifold_renderer::node_graph::loaded_preset_view_by_id(fx.effect_type())?;
                full_reshape_from_def(&view.canonical_def, param_id)
            }
            manifold_core::GraphTarget::Generator(lid) => {
                let layer = self
                    .local_project
                    .timeline
                    .layers
                    .iter()
                    .find(|l| &l.layer_id == lid)?;
                if let Some(def) = layer.generator_graph()
                    && let Some(r) = full_reshape_from_def(def, param_id)
                {
                    return Some(r);
                }
                let gp = layer.gen_params()?;
                let view =
                    manifold_renderer::node_graph::loaded_preset_view_by_id(gp.generator_type())?;
                full_reshape_from_def(&view.canonical_def, param_id)
            }
        }
    }

    /// The card binding (if any) governing `(node_id, param_name)` on the
    /// watched graph at `scope_path` — BUG-158 write-back's binding lookup
    /// (D1). Same override-then-canonical fallback as
    /// [`Self::watched_full_reshape`]: most bound params are still on the
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
        match self.watched_graph_target.as_ref()? {
            manifold_core::GraphTarget::Effect(eid) => {
                let fx = self.local_project.find_effect_by_id(eid)?;
                if let Some(def) = fx.graph.as_ref()
                    && let Some(r) = binding_for_node_param(def, scope_path, node_id, param_name)
                {
                    return Some(r);
                }
                let view =
                    manifold_renderer::node_graph::loaded_preset_view_by_id(fx.effect_type())?;
                binding_for_node_param(&view.canonical_def, scope_path, node_id, param_name)
            }
            manifold_core::GraphTarget::Generator(lid) => {
                let layer = self
                    .local_project
                    .timeline
                    .layers
                    .iter()
                    .find(|l| &l.layer_id == lid)?;
                if let Some(def) = layer.generator_graph()
                    && let Some(r) = binding_for_node_param(def, scope_path, node_id, param_name)
                {
                    return Some(r);
                }
                let gp = layer.gen_params()?;
                let view =
                    manifold_renderer::node_graph::loaded_preset_view_by_id(gp.generator_type())?;
                binding_for_node_param(&view.canonical_def, scope_path, node_id, param_name)
            }
        }
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
        let def = match target {
            manifold_core::GraphTarget::Effect(eid) => {
                let fx = self.local_project.find_effect_by_id(eid)?;
                fx.graph.as_ref().unwrap_or(catalog_default)
            }
            manifold_core::GraphTarget::Generator(lid) => {
                let layer = self
                    .local_project
                    .timeline
                    .layers
                    .iter()
                    .find(|l| &l.layer_id == lid)?;
                layer.generator_graph().unwrap_or(catalog_default)
            }
        };
        node_param_value(def, scope_path, node_id, param_name)
    }

    /// `true` when `(node_id, param_name)` on the watched graph at
    /// `scope_path` has an incoming wire — same override-then-canonical
    /// fallback as [`Self::watched_binding_for_node_param`]. The P1
    /// enforcement backstop (Invariants §4): a wired param is never rerouted
    /// through the card write-back path, matching D5/D6 (wire beats binding).
    pub(crate) fn watched_node_param_is_wired(&self, scope_path: &[u32], node_id: u32, param_name: &str) -> bool {
        let Some(target) = self.watched_graph_target.as_ref() else {
            return false;
        };
        match target {
            manifold_core::GraphTarget::Effect(eid) => {
                let Some(fx) = self.local_project.find_effect_by_id(eid) else {
                    return false;
                };
                if let Some(def) = fx.graph.as_ref()
                    && node_param_is_wired(def, scope_path, node_id, param_name)
                {
                    return true;
                }
                manifold_renderer::node_graph::loaded_preset_view_by_id(fx.effect_type())
                    .is_some_and(|view| node_param_is_wired(&view.canonical_def, scope_path, node_id, param_name))
            }
            manifold_core::GraphTarget::Generator(lid) => {
                let Some(layer) = self.local_project.timeline.layers.iter().find(|l| &l.layer_id == lid)
                else {
                    return false;
                };
                if let Some(def) = layer.generator_graph()
                    && node_param_is_wired(def, scope_path, node_id, param_name)
                {
                    return true;
                }
                let Some(gp) = layer.gen_params() else {
                    return false;
                };
                manifold_renderer::node_graph::loaded_preset_view_by_id(gp.generator_type())
                    .is_some_and(|view| node_param_is_wired(&view.canonical_def, scope_path, node_id, param_name))
            }
        }
    }

    /// The catalog graph def to seed the instance's per-instance graph
    /// override when a reshape edit hits an instance still on the catalog
    /// default. Effects start on the catalog default (`graph: None`), so they
    /// need the seed; a generator layer always carries its authoring
    /// `generator_graph`, so the seed is only a safety net. `None` when the
    /// instance has already diverged (no seed needed) or can't be resolved.
    fn seed_def_for(
        &self,
        target: &manifold_core::GraphTarget,
    ) -> Option<manifold_core::effect_graph_def::EffectGraphDef> {
        seed_def_for_project(&self.local_project, target)
    }

    /// The graph def the editor currently shows for the watched target — the
    /// per-instance override if it has diverged, else the catalog / bundled
    /// default. Cloned (copy is a rare authoring action). `None` when nothing is
    /// watched.
    pub(crate) fn watched_def_cloned(&self) -> Option<manifold_core::effect_graph_def::EffectGraphDef> {
        match self.watched_graph_target.as_ref()? {
            manifold_core::GraphTarget::Effect(eid) => {
                let fx = self.local_project.find_effect_by_id(eid)?;
                if let Some(d) = fx.graph.as_ref() {
                    Some(d.clone())
                } else {
                    manifold_renderer::node_graph::loaded_preset_view_by_id(fx.effect_type())
                        .map(|v| (*v.canonical_def).clone())
                }
            }
            manifold_core::GraphTarget::Generator(lid) => {
                let layer = self
                    .local_project
                    .timeline
                    .layers
                    .iter()
                    .find(|l| &l.layer_id == lid)?;
                if let Some(d) = layer.generator_graph() {
                    Some(d.clone())
                } else {
                    manifold_renderer::node_graph::bundled_preset_def(
                        layer.gen_params()?.generator_type(),
                    )
                    .cloned()
                }
            }
        }
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

    /// Read the watched param's CURRENT (post-modulation) value — the number
    /// shown on the card slider — for the mapping popover's live dot. Reads the
    /// same `param_values` slot drivers / Ableton / envelopes write each frame,
    /// so the dot tracks live motion. `None` if the param doesn't resolve.
    pub(crate) fn watched_value(&self, param_id: &str) -> Option<f32> {
        match self.watched_graph_target.as_ref()? {
            manifold_core::GraphTarget::Effect(eid) => {
                let fx = self.local_project.find_effect_by_id(eid)?;
                fx.params.get(param_id).map(|p| p.value)
            }
            manifold_core::GraphTarget::Generator(lid) => {
                let gp = self
                    .local_project
                    .timeline
                    .layers
                    .iter()
                    .find(|l| &l.layer_id == lid)?
                    .gen_params()?;
                gp.params.get(param_id).map(|p| p.value)
            }
        }
    }

    /// Live-drag preview: apply the partial edit to the watched param's
    /// reshape store on BOTH the local project (immediate card UI) and the
    /// content thread (smooth canvas + survives the next snapshot sync),
    /// WITHOUT recording undo — the commit records the single undo entry.
    /// Reuses the edit command's own apply logic (it picks user-binding vs
    /// note + seeds copy-on-write), so the preview can never diverge from
    /// the commit.
    pub(crate) fn preview_mapping(
        &mut self,
        target: &manifold_core::GraphTarget,
        param_id: &str,
        edit: BindingMappingEdit,
    ) {
        let seed_def = self.seed_def_for(target);
        build_mapping_command(target, param_id, edit.clone(), seed_def.clone())
            .execute(&mut self.local_project);
        let target = target.clone();
        let pid = param_id.to_string();
        self.send_content_cmd(ContentCommand::MutateProject(Box::new(move |p| {
            build_mapping_command(&target, &pid, edit, seed_def).execute(p);
        })));
    }

    /// Commit / single-shot: send the reshape edit as one undoable command.
    /// Self-captures the reverse (correct for single-shot — nothing mutated
    /// the store first).
    pub(crate) fn commit_mapping(
        &mut self,
        target: &manifold_core::GraphTarget,
        param_id: &str,
        edit: BindingMappingEdit,
    ) {
        let seed_def = self.seed_def_for(target);
        self.send_content_cmd(ContentCommand::Execute(build_mapping_command(
            target, param_id, edit, seed_def,
        )));
    }

    /// Drag commit: one undoable command carrying the explicit pre-drag
    /// reverse, so undo restores the pre-drag values rather than the
    /// preview-mutated ones.
    pub(crate) fn commit_mapping_with_reverse(
        &mut self,
        target: &manifold_core::GraphTarget,
        param_id: &str,
        new: BindingMappingEdit,
        reverse: BindingMappingEdit,
    ) {
        let seed_def = self.seed_def_for(target);
        self.send_content_cmd(ContentCommand::Execute(build_mapping_command_with_reverse(
            target, param_id, new, reverse, seed_def,
        )));
    }

    /// Resolve an effect card's row index (in the active inspector tab) to its
    /// stable [`EffectId`]. Mirrors the cog's `OpenGraphEditor` resolution:
    /// keyed by instance id, tab-aware (Master / Layer|Group / Clip). Takes
    /// `&mut self` only because the Clip-tab lookup runs through
    /// `Timeline::find_clip_by_id`, which self-heals its location cache.
    pub(crate) fn resolve_effect_card_id(&mut self, ei: usize) -> Option<manifold_core::EffectId> {
        match self.ws.ui_root.inspector.last_effect_tab() {
            manifold_ui::InspectorTab::Master => self
                .local_project
                .settings
                .master_effects
                .get(ei)
                .map(|e| e.id.clone()),
            manifold_ui::InspectorTab::Layer | manifold_ui::InspectorTab::Group => self
                .active_layer_id
                .as_ref()
                .and_then(|id| self.local_project.timeline.find_layer_by_id(id))
                .and_then(|(_, l)| l.effects.as_ref())
                .and_then(|effects| effects.get(ei))
                .map(|e| e.id.clone()),
            manifold_ui::InspectorTab::Clip => self
                .selection
                .primary_selected_clip_id
                .as_ref()
                .and_then(|cid| self.local_project.timeline.find_clip_by_id(cid))
                .and_then(|c| c.effects.get(ei))
                .map(|e| e.id.clone()),
        }
    }

    /// Point the graph editor at an effect instance: push the watched identity
    /// to the content thread and cache its catalog-default graph def (so the
    /// per-card edit commands can lift `instance.graph` from `None` on first
    /// edit). Does NOT open the window — shared by the card cog (which also
    /// sets `pending_open_graph_editor`) and selection-follows (retarget-only).
    pub(crate) fn watch_effect_graph(&mut self, effect_id: manifold_core::EffectId) {
        self.send_content_cmd(ContentCommand::WatchEffectGraph(Some(effect_id.clone())));
        self.watched_catalog_default = self.local_project.find_effect_by_id(&effect_id).and_then(
            |instance| manifold_renderer::node_graph::catalog_graph_def_for(instance.effect_type()),
        );
        self.watched_graph_target = Some(manifold_core::GraphTarget::Effect(effect_id));
    }

    /// Point the graph editor at a layer's generator graph. Caches the bundled
    /// preset JSON for the layer's generator type as the catalog default. Does
    /// NOT open the window — shared by the generator-card cog and
    /// selection-follows.
    pub(crate) fn watch_generator_graph(&mut self, layer_id: manifold_core::LayerId) {
        self.send_content_cmd(ContentCommand::WatchGeneratorGraph(Some(layer_id.clone())));
        self.watched_catalog_default = self
            .local_project
            .timeline
            .find_layer_by_id(&layer_id)
            .map(|(_, l)| l.generator_type().clone())
            .filter(|gt| !gt.is_none())
            .and_then(|gt| manifold_renderer::node_graph::bundled_preset_json(&gt))
            .and_then(|json| serde_json::from_str(&json).ok());
        self.watched_graph_target = Some(manifold_core::GraphTarget::Generator(layer_id));
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
        let src = self.content_state.active_graph_snapshot.as_ref()?;
        let fresh =
            matches!(&self.editor_ui_graph, Some((cached, _)) if std::sync::Arc::ptr_eq(cached, src));
        if !fresh {
            let ui = std::sync::Arc::new(crate::ui_translate::graph_snapshot_to_ui(src));
            self.editor_ui_graph = Some((src.clone(), ui));
        }
        self.editor_ui_graph.as_ref().map(|(_, ui)| ui.clone())
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
        let preview_node = match (self.graph_canvas.as_ref(), editor_ui_snap.as_deref()) {
            (Some(canvas), Some(snap)) => canvas
                .selected_node_id()
                .and_then(|id| resolve_preview_target(snap, canvas.scope_path(), id)),
            _ => None,
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

        // P5c (`docs/REALTIME_3D_DESIGN.md`): resolve whether `preview_node`
        // qualifies for the 3D viewport (a top-level `node.render_scene`
        // node — `find_snapshot_node` recurses into groups looking for the
        // type, but `override_camera_def` below only splices into a node
        // found in the def's FLAT top-level `nodes` list, so a nested
        // render_scene fails to open with `RenderSceneNodeNotFound`, a known
        // P5 constraint) and — only if so and the viewport is toggled open —
        // clone its def, BEFORE the editor workspace is borrowed mutably
        // below. `watched_def_cloned()` takes `&self` whole, which would
        // conflict with `ws`'s later mutable borrow of `self.graph_editor`
        // (same reason `popover_live_value` is resolved before `ws`,
        // further down).
        let viewport_is_scene_node = self.last_preview_node.as_ref().is_some_and(|id| {
            self.content_state
                .active_graph_snapshot
                .as_deref()
                .and_then(|s| find_snapshot_node(&s.nodes, id))
                .is_some_and(|n| n.type_id == "node.render_scene")
        });
        let viewport_open = self.graph_editor.as_ref().is_some_and(|ed| ed.viewport_open);
        let viewport_def = if viewport_is_scene_node && viewport_open {
            self.watched_def_cloned()
        } else {
            None
        };

        // Send the canvas's currently-visible nodes (deduped) so the content
        // thread captures thumbnails only for what's on screen — hidden /
        // off-scope / collapsed-group nodes cost nothing. The set is the whole
        // current scope level (what the atlas shows), so it changes only on a
        // scope descend/ascend or a topology edit, not on pan/zoom.
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

        // ── Mini-timeline data ────────────────────────────────────────
        // Built from the UI project mirror + playhead BEFORE the disjoint `ws`
        // borrow below. Cheap: walks the clip list once, off the per-node hot
        // path. Shared with the headless snapshot path via `mini_timeline_data`.
        let mini_current_beat = self.content_state.current_beat.as_f32();
        let mini_is_playing = self.content_state.is_playing;
        let (mini_clips, mini_layer_labels, mini_rows, mini_total_beats, mini_beats_per_bar, mini_readout) =
            mini_timeline_data(&self.local_project, mini_current_beat);

        let Some(gpu) = &self.gpu else { return };
        let Some(wid) = self.graph_editor_window_id else {
            return;
        };
        // Resolve the open popover's live value before borrowing the editor
        // window state mutably (`watched_value` borrows all of `self`).
        let popover_live_value = if self.editor_mapping_popover.is_open() {
            self.watched_value(self.editor_mapping_popover.binding_id())
        } else {
            None
        };
        let Some(ws) = self.graph_editor.as_mut() else {
            return;
        };

        // `EDITOR_WINDOW_UNIFICATION_DESIGN.md` D6: the same aggregate the
        // main window ORs into its own `offscreen_dirty` above (tick_and_
        // render's step 6·motion) — one predicate, both windows, no per-
        // window keepalive list. This window's own `UIRoot` never opens the
        // toast today (only `self.ws.ui_root.toast.show()` is ever called),
        // so this is currently a no-op read, kept for when it does — and
        // this window recomposites every vsync pulse regardless (D5,
        // cacheless), so `offscreen_dirty` isn't a redraw gate here the way
        // it is on the main window; this OR keeps the two windows' policy
        // identical rather than special-casing the editor's redraw path.
        if ws.ui_root.overlay_redraw_needed() {
            ws.offscreen_dirty = true;
        }

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
        // Left preview sidebar (monitors) + center canvas + right card lane
        // (param expose) — card on the right, same convention as the main
        // timeline's inspector (`inspector_width` docks at `screen_width -
        // inspector_width`). Built BEFORE rendering so the tree's nodes
        // (panels + buttons + labels) are ready to draw alongside the canvas.
        // Column geometry comes from the workspace's resizable `Dock` (left
        // preview column + right card lane). One `rects()` call feeds render,
        // `editor_canvas_viewport`, and the pointer handlers alike, so the
        // canvas origin and click hit-testing stay in lockstep no matter how
        // the user drags the dividers.
        let editor_area = manifold_ui::Rect::new(0.0, 0.0, logical_w as f32, logical_h as f32);
        let dock = ws.dock.rects(editor_area);
        let preview_width = dock.left.width;
        let card_width = dock.right.width;
        let canvas_x = dock.canvas.x;
        let canvas_width = dock.canvas.width;
        // Canvas height is short of the window by the bottom mini-timeline strip
        // (via the same `dock` the input pass reads, so hit-testing tracks it).
        let canvas_height = dock.canvas.height;
        let card_x = dock.right.x;
        // When a node is being previewed, the preview pane occupies the top of
        // the sidebar; the expose/param rows start below it so they don't
        // overlap. Logical units; the present pass draws the pane to match.
        let preview_pad = 8.0_f32;
        let preview_title_h = 18.0_f32;
        // Monitors take the project aspect ratio (a portrait show → portrait
        // monitors), fit to the column width and clamped in height so the two
        // stacked panes always fit the editor window, never spilling onto the
        // chrome below. Landscape stays full-width (unchanged); portrait is
        // narrower and centred in the column.
        let monitor_aspect = self
            .content_pipeline_output
            .as_ref()
            .map(|p| p.get_dimensions())
            .filter(|(_, h)| *h > 0)
            .map(|(w, h)| w as f32 / h as f32)
            .unwrap_or(16.0 / 9.0);
        let avail_w = (preview_width - 2.0 * preview_pad).max(1.0);
        // Height budget so 2×(title + body) + 3 pads fits the column vertically.
        // `canvas_height` (not the full window) — the column stops above the
        // full-width mini-timeline strip.
        let max_body_h =
            ((canvas_height - 3.0 * preview_pad - 2.0 * preview_title_h) * 0.5).max(1.0);
        let width_bound_h = avail_w / monitor_aspect;
        let (preview_w, preview_h) = if width_bound_h <= max_body_h {
            (avail_w, width_bound_h)
        } else {
            (max_body_h * monitor_aspect, max_body_h)
        };
        let preview_x = (preview_width - preview_w) * 0.5;
        // The left column is monitors-only now — the inner-node param list moved
        // under the right card. Two equal stacked 16:9 monitors (the selected
        // node's output on top, the master compositor output below), each a
        // titled pane (title row + body). The pair is centred vertically in the
        // column so it reads as intentional rather than top-pinned with a void
        // beneath. The node pane's body shows the node's image, or — for a
        // control / math / envelope node with no image — its value inspector text
        // in place of the image, or a placeholder when nothing is selected. The
        // master pane always shows what the live show is putting out.
        let node_preview_info = self.content_state.node_preview_info.clone();
        let preview_has_image = node_preview_info
            .as_ref()
            .map(|i| i.has_image)
            .unwrap_or(false);
        let show_image = self.last_preview_node.is_some() && preview_has_image;
        let pane_block_h = 2.0 * (preview_title_h + preview_h) + preview_pad;
        let mut pane_y = ((canvas_height - pane_block_h) * 0.5).max(preview_pad);
        // Node-output monitor: title row + project-aspect body.
        let node_title_y = pane_y;
        let node_img_y = node_title_y + preview_title_h;
        pane_y = node_img_y + preview_h + preview_pad;
        // Master-out monitor: just below the node pane.
        let master_title_y = pane_y;
        let master_img_y = master_title_y + preview_title_h;
        let card_viewport = manifold_ui::Rect::new(card_x, 0.0, card_width, canvas_height);

        // P5c (`docs/REALTIME_3D_DESIGN.md`): open/rebuild/sync the 3D
        // viewport session and reserve its screen rect — the SAME rect the
        // 2D node-output monitor above occupies, so the viewport slots into
        // existing dock geometry rather than adding new dock UI. Render
        // target pixel size tracks the pane at the window's own scale
        // factor, same crisp-at-any-size convention the audio spectrogram
        // uses. `viewport_def` was cloned before `ws` was borrowed (see the
        // comment above its `let` near the top of this function); the
        // fields read here (`self.primitive_registry`, `gpu.device`,
        // `self.content_state`, `self.time_since_start`) are all direct
        // field accesses on OTHER fields than `self.graph_editor`, so they
        // coexist with `ws`'s mutable borrow without conflict.
        let viewport_scale = scale as f32;
        let viewport_tex_w = ((preview_w * viewport_scale).round() as u32).clamp(64, 4096);
        let viewport_tex_h = ((preview_h * viewport_scale).round() as u32).clamp(64, 4096);
        let viewport_ctx = manifold_renderer::preset_context::PresetContext {
            time: self.time_since_start as f64,
            beat: mini_current_beat as f64,
            dt,
            width: viewport_tex_w,
            height: viewport_tex_h,
            output_width: viewport_tex_w,
            output_height: viewport_tex_h,
            aspect: viewport_tex_w as f32 / (viewport_tex_h.max(1) as f32),
            owner_key: 0,
            is_clip_level: false,
            frame_count: (self.time_since_start as f64 * 60.0) as i64,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        if let Some(def) = viewport_def.as_ref() {
            // `viewport_def` is only `Some` when `viewport_is_scene_node` held,
            // which requires `self.last_preview_node` to be `Some` — see its
            // `let` above.
            let render_scene_node = self.last_preview_node.clone().expect(
                "viewport_def is only Some when viewport_is_scene_node held, which requires last_preview_node",
            );
            let needs_open = match ws.viewport_session.as_ref() {
                Some(s) => s.dimensions() != (viewport_tex_w, viewport_tex_h),
                None => true,
            };
            if needs_open {
                match manifold_renderer::node_graph::ViewportSession::open(
                    def,
                    &render_scene_node,
                    &self.primitive_registry,
                    std::sync::Arc::clone(&gpu.device),
                    viewport_tex_w,
                    viewport_tex_h,
                    &viewport_ctx,
                ) {
                    Ok(session) => ws.viewport_session = Some(session),
                    Err(_e) => {
                        // no-silent-fallbacks: a failed splice (e.g. a nested
                        // render_scene node, the known P5 constraint above)
                        // clears the session so the pane shows nothing rather
                        // than a stale or wrong frame.
                        ws.viewport_session = None;
                        ws.viewport_pane = None;
                    }
                }
            } else if let Some(session) = ws.viewport_session.as_mut()
                && session.sync_def(def, &self.primitive_registry, &viewport_ctx).is_err()
            {
                ws.viewport_session = None;
                ws.viewport_pane = None;
            }
        } else {
            ws.viewport_session = None;
            ws.viewport_pane = None;
        }
        ws.viewport_rect = ws
            .viewport_session
            .is_some()
            .then(|| manifold_ui::Rect::new(preview_x, node_img_y, preview_w, preview_h));

        // Render if dirty (camera moved this frame, or the session was just
        // (re)built above — never per display tick: `render_if_dirty` is a
        // no-op cache hit unless `ViewportSession`'s own `dirty` flag is set,
        // which only navigation input (`viewport_input::apply`, below) or a
        // def change sets) and upload into the UI-device-local pane the
        // present pass blits below — same `TexturePane::local` + `upload_texture`
        // pattern the audio spectrogram uses (`ui_frame.rs`).
        if let Some(session) = ws.viewport_session.as_mut() {
            let (w, h) = session.dimensions();
            // P6: gizmo handle geometry for the current selection/mode,
            // built against THIS frame's def and editor camera — see
            // `viewport_gizmo` and the `ws.viewport_selected_object`/
            // `ws.viewport_gizmo_mode` doc comments (`workspace.rs`).
            let gizmo_lines: Vec<manifold_renderer::node_graph::WorldLine> = viewport_def
                .as_ref()
                .and_then(manifold_renderer::node_graph::scene_vm::SceneVm::from_def)
                .zip(ws.viewport_selected_object)
                .and_then(|(scene, object_id)| manifold_renderer::node_graph::gizmo_target_for(&scene, object_id))
                .map(|target| manifold_renderer::node_graph::gizmo_lines(ws.viewport_gizmo_mode, &target))
                .unwrap_or_default();
            let rgba = session.render_if_dirty(
                &viewport_ctx,
                &manifold_renderer::node_graph::ViewportOverlayConfig::default(),
                None,
                &[],
                &gizmo_lines,
            );
            let need_new_tex = !matches!(
                ws.viewport_pane.as_ref().and_then(|p| p.local_target()),
                Some(tex) if tex.width == w && tex.height == h
            );
            if need_new_tex {
                let tex = gpu.device.create_texture(&manifold_gpu::GpuTextureDesc {
                    width: w,
                    height: h,
                    depth: 1,
                    format: manifold_gpu::GpuTextureFormat::Rgba8Unorm,
                    dimension: manifold_gpu::GpuTextureDimension::D2,
                    usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
                    label: "3D Viewport",
                    mip_levels: 1,
                });
                ws.viewport_pane = Some(crate::texture_pane::TexturePane::local(tex));
            }
            if let Some(pane) = ws.viewport_pane.as_ref()
                && let Some(tex) = pane.local_target()
            {
                gpu.device.upload_texture(tex, &rgba);
            }
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
        let node_inspector = node_preview_info
            .as_ref()
            .filter(|i| !i.has_image)
            .map(|info| {
                let snap_node = snap_arc
                    .as_deref()
                    .and_then(|s| find_snapshot_node(&s.nodes, &info.node_id));
                let title = snap_node
                    .map(|n| n.title.clone())
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| info.node_id.to_string());
                let description = snap_node
                    .and_then(|n| {
                        manifold_renderer::node_graph::descriptor_for(&n.type_id)
                    })
                    .map(|d| {
                        if !d.summary.is_empty() {
                            d.summary.to_string()
                        } else {
                            // First sentence of the technical purpose keeps it short.
                            d.purpose.split(". ").next().unwrap_or(d.purpose).to_string()
                        }
                    })
                    .unwrap_or_default();
                manifold_ui::panels::graph_editor::NodeInspector {
                    title,
                    description,
                    inputs: info.inputs.clone(),
                    outputs: info.outputs.clone(),
                }
            });
        self.graph_editor_panel.set_node_inspector(node_inspector);

        // Right lane = the WHOLE main-window inspector column (master/layer/clip
        // tabs, every effect card, generator params, macros, chrome). Same panel
        // type as the main window, this window's own `ws.ui_root.inspector`
        // instance, driven by the same `Arc<Project>` snapshot — configured in the
        // main tick in lockstep with `self.ws`'s inspector, so here we only lay it
        // out into the right lane. Selecting a card retargets the canvas; edits
        // mirror both windows next snapshot. Replaces the single watched
        // `editor_card`. See docs/GRAPH_EDITOR_INSPECTOR_UNIFICATION.md (Change 3).

        // Tick parity (`GRAPH_EDITOR_INSPECTOR_UNIFICATION.md` Change 4, D4):
        // the editor's `UIRoot` never sets `built`, so its own `update()`
        // early-returns and would otherwise never advance drawer-height
        // tweens / value-flash / fire meters — the card frame would sit at
        // a stale height while the rows below build at their true size and
        // overflow it (BUG-160). `tick_inspector` runs before the rebuild
        // below so the advanced values are what this frame's `build_...`
        // reads (mirrors the main window: `self.ws.ui_root.update()` then
        // its own rebuild-on-dirty poll). `update_fire_meters` mirrored
        // alongside it, same as the main window's call
        // (`self.ws.ui_root.update_fire_meters`, above in this function's
        // caller) — this also retires BUG-157's inspector half.
        ws.ui_root.tick_inspector();
        ws.ui_root.update_fire_meters(&self.content_state.fire_meters, dt);

        // Per-frame card VALUE sync for the editor's inspector column — the
        // same call `push_state` makes for the main window (step 4 of the
        // main tick), against the same `local_project` + active layer, so a
        // driver / mapping / envelope moving a knob is seen on this window's
        // card sliders this frame instead of freezing until the next
        // structural sync. Drag safety rides on the shared guard: the
        // snapshot drain already restored the actively-dragged field into
        // `local_project`, so this writes the user's own value straight back.
        let editor_active_idx = self
            .active_layer_id
            .as_ref()
            .and_then(|id| self.local_project.timeline.find_layer_index_by_id(id));
        crate::ui_bridge::sync_card_values(&mut ws.ui_root, &self.local_project, editor_active_idx);
        crate::ui_bridge::sync_scene_row_values(&mut ws.ui_root, &self.local_project);

        // Rebuild the editor's UITree from scratch each frame: tree state
        // is small, so a clear + rebuild is cheaper than dirty-tracking and
        // means stale rows can never linger after the target changes.
        ws.ui_root.tree.clear();
        ws.ui_root.build_inspector_in_rect(card_viewport);
        let _ = (card_x, card_width);

        // Pinned preview monitors in the left column: a backing panel, the two pane
        // titles, and — for a non-image node — the value inspector text in the
        // node pane. Added to the editor tree so they composite into the
        // offscreen; the monitor images blit onto the drawable below each title
        // in the present pass.
        // Shared with the headless editor harness (`editor_frame.rs`,
        // `UI_HARNESS_UNIFICATION_DESIGN.md` P3) — see that module's doc for
        // why threading the already-configured `graph_editor_panel` (not the
        // raw content-state fields feeding it) is the non-entangled cut.
        self.editor_smart_preview_toggle_id = crate::editor_frame::build_editor_preview_column(
            &mut ws.ui_root.tree,
            &self.graph_editor_panel,
            preview_width,
            canvas_height,
            preview_x,
            preview_w,
            preview_h,
            node_title_y,
            node_img_y,
            master_title_y,
            show_image,
        );

        // Overlays (node picker + any other top-level overlay open on this
        // window's `UIRoot`) build at the tail of the tree via the SAME
        // driver the main window uses — `build_overlays_for_screen`
        // sets `screen_width`/`screen_height` (this `UIRoot` never gets
        // `resize()`) then runs `build_overlays()`, which records
        // `overlay_draw`/`overlay_region_start` for real.
        // Built last, same as before, so its nodes sit on top of
        // palette/sidebar/preview column.
        ws.ui_root.build_overlays_for_screen(logical_w as f32, logical_h as f32);

        // The editor rebuilt its whole tree above, clearing every node's flags.
        // Re-apply HOVERED / PRESSED from the input system's durable widget state
        // so interaction visuals (button hover/press) persist across the per-frame
        // rebuild instead of flickering off until the next pointer move.
        {
            let ui_root = &mut ws.ui_root;
            ui_root.input.apply_interaction_flags(&mut ui_root.tree);
        }

        // A pending jump-to-node centres now that `set_snapshot` has laid the
        // canvas out for this frame's scope (no-op until the target is present).
        if let Some(canvas) = self.graph_canvas.as_mut() {
            let vp = crate::graph_canvas::Rect::new(canvas_x, 0.0, canvas_width, canvas_height);
            canvas.resolve_pending_focus(vp);
            // Frame the whole level on editor open / scope change (camera only).
            canvas.apply_pending_fit(vp);
            // D17 "wire→port magnetize": needs `vp` (port screen positions),
            // which the main `canvas.tick(dt_ms)` call site (§1c above)
            // doesn't have — ticked here instead, right before the draw
            // pass that reads it.
            canvas.tick_wire_magnet(vp);
        }

        // ── Node output previews (BUG-027): register the current atlas front
        // under a fixed handle and hand each visible node its cell UV, so the
        // canvas paints the preview inline at the node's own depth band (where a
        // node stacked above occludes it) instead of the old flat post-pass blit
        // that ignored node z-order. Phase A reads (build the map + clone the
        // atlas texture); phase B is the two mutable installs.
        #[cfg(target_os = "macos")]
        {
            let atlas_handle =
                manifold_ui::node::texture_handle_for_key("__manifold_graph_node_atlas__");
            let prepared = if let (Some(bridge), Some(canvas)) = (
                self.node_atlas_texture_bridge.as_ref(),
                self.graph_canvas.as_ref(),
            ) {
                let front = bridge.front_index() as usize;
                self.ui_node_atlas_textures
                    .get(front)
                    .and_then(|t| t.as_ref())
                    .map(|atlas_tex| {
                        let layout: ahash::AHashMap<&manifold_core::NodeId, u32> = self
                            .content_state
                            .node_atlas_layout
                            .iter()
                            .map(|(id, cell)| (id, *cell))
                            .collect();
                        let vp = crate::graph_canvas::Rect::new(
                            canvas_x,
                            0.0,
                            canvas_width,
                            logical_h as f32,
                        );
                        // Each source is letterboxed into its 16:9 cell; the
                        // on-canvas screen takes the project aspect, so sample only
                        // the letterboxed content sub-rect. Edge-straddle clipping
                        // is now the canvas viewport scissor, so (unlike the old
                        // blit) no per-node UV cropping is needed here.
                        let mut map: ahash::AHashMap<
                            manifold_core::NodeId,
                            (manifold_ui::node::TextureHandle, [f32; 4]),
                        > = ahash::AHashMap::new();
                        for (node_id, _, _, _, _) in canvas.visible_node_thumbnails(vp) {
                            let Some(&cell) = layout.get(&node_id) else {
                                continue;
                            };
                            let uv = crate::content_pipeline::atlas_cell_uv(cell, monitor_aspect);
                            map.insert(node_id, (atlas_handle, uv));
                        }
                        (atlas_tex.clone(), map)
                    })
            } else {
                None
            };
            if let Some((atlas_tex, map)) = prepared {
                if let Some(ui) = self.ui_renderer.as_mut() {
                    ui.register_external_texture(atlas_handle, atlas_tex);
                }
                if let Some(canvas) = self.graph_canvas.as_mut() {
                    canvas.set_node_preview_src(map);
                }
            }
        }

        // ── Build frame: clear, then draw the canvas + sidebar ──
        // Shared with the headless editor harness — see `editor_frame.rs`
        // module doc (P3). `composite_editor_frame` owns the encoder
        // create+commit internally, mirroring `ui_frame::composite_main_ui_frame`.
        crate::editor_frame::composite_editor_frame(
            &gpu.device,
            self.ui_renderer.as_mut(),
            &mut ws.ui_root,
            &ws.dock,
            editor_area,
            self.graph_canvas.as_ref(),
            crate::graph_canvas::Rect::new(canvas_x, 0.0, canvas_width, canvas_height),
            crate::editor_frame::EditorMiniTimelineInputs {
                bottom_rect: dock.bottom,
                show_bottom: ws.dock.show_bottom,
                total_beats: mini_total_beats,
                beats_per_bar: mini_beats_per_bar,
                current_beat: mini_current_beat,
                row_count: mini_rows,
                clips: &mini_clips,
                layer_labels: &mini_layer_labels,
                readout: &mini_readout,
                is_playing: mini_is_playing,
            },
            &mut self.editor_mapping_popover,
            popover_live_value,
            &self.text_input,
            &self.frame_timer,
            offscreen,
            logical_w,
            logical_h,
            scale,
        );
        ws.offscreen_dirty = false;

        // Skip drawable acquisition on the resize frame — drawable pool
        // may still be reconfiguring.
        if ws.surface_resized_this_frame {
            ws.surface_resized_this_frame = false;
            return;
        }

        // ── Late drawable acquisition + blit ──
        let Some(drawable) = surface.next_drawable() else {
            return;
        };
        let drawable_tex = drawable.gpu_texture(manifold_gpu::GpuTextureFormat::Bgra8Unorm);
        let (Some(blit_p), Some(blit_s)) = (&self.blit_pipeline, &self.blit_sampler) else {
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
        // targets the drawable's Bgra8Unorm format, so these draw into the
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
                let front = bridge.front_index() as usize;
                if let Some(tex) = self
                    .ui_node_preview_textures
                    .get(front)
                    .and_then(|t| t.as_ref())
                {
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
            // P5c 3D viewport — the persistent `ViewportSession`'s composited
            // RGBA8 (scene + D7 overlays), uploaded into a UI-device-local
            // pane above and blit here through the same unified
            // `TexturePane` path the audio spectrogram uses. `Local`, never
            // an IOSurface bridge: the session rendered on THIS (editor UI)
            // thread just above, so there's no cross-thread hand-off.
            if let Some(pane) = ws.viewport_pane.as_mut() {
                crate::texture_pane::blit_texture_pane(
                    pane,
                    &gpu.device,
                    &mut present_enc,
                    blit_p,
                    blit_s,
                    &drawable_tex,
                    (preview_x, node_img_y, preview_w, preview_h),
                    scale,
                    "Viewport → Sidebar",
                );
            }
            // Master-out monitor — the live compositor output, the same texture
            // the main/perform window presents. Imported into `ui_preview_textures`
            // by the workspace-preview present path that runs just before this.
            if let Some(bridge) = self.preview_texture_bridge.as_ref() {
                let front = bridge.front_index() as usize;
                if let Some(tex) = self.ui_preview_textures.get(front).and_then(|t| t.as_ref()) {
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
        present_enc.commit();
    }
}

#[cfg(test)]
mod preview_target_tests {
    use super::resolve_preview_target;
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
    use manifold_core::NodeId;
    use manifold_core::effect_graph_def::{
        BindingDef, BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire, ParamSpecDef,
        PresetMetadata,
    };
    use manifold_core::macro_bank::MacroCurve;
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
                id: manifold_core::preset_type_id::PresetTypeId::new("Test"),
                display_name: "Test".into(),
                category: "Test".into(),
                osc_prefix: "test".into(),
                legacy_discriminant: None,
                available: true,
                is_line_based: false,
                params: vec![ParamSpecDef {
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
                }],
                skip_mode: Default::default(),
                param_aliases: vec![],
                value_aliases: vec![],
                string_params: vec![],
                string_bindings: vec![],
            }),
            nodes: vec![node(1, "blur1")],
            wires: vec![],
        }
    }

    #[test]
    fn binding_for_node_param_resolves_and_inverts() {
        let def = def_with_binding();
        let (outer_id, min, max, invert, curve, scale, offset) =
            binding_for_node_param(&def, &[], 1, "amount").expect("binding resolves");
        assert_eq!(outer_id, "amount");
        let target =
            manifold_core::effects::apply_card_reshape(0.5, min, max, invert, curve, scale, offset);
        let back =
            manifold_core::effects::invert_card_reshape(target, min, max, invert, curve, scale, offset)
                .expect("non-degenerate scale");
        assert!((back - 0.5).abs() < 1e-3, "expected ~0.5, got {back}");
    }

    #[test]
    fn binding_for_node_param_none_when_unbound() {
        let def = def_with_binding();
        assert!(binding_for_node_param(&def, &[], 1, "other_param").is_none());
        assert!(binding_for_node_param(&def, &[], 99, "amount").is_none());
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
