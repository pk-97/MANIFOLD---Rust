//! Gizmo drags as knob scrubs (REALTIME_3D_DESIGN.md D8): a gizmo handle
//! writes each param it names exactly as that param's knob does. A param
//! bound to a card scrubs the card (`ValueRef::Param`); an unbound node
//! param scrubs as the graph node face does; a direction's three card
//! params move as one gesture. Live writes while dragging, one undo entry
//! on release, no private path.
//!
//! The authored graph projection here is what the viewport's `SceneVm`
//! reads: card values folded into the params they drive, so handles sit at
//! the values the knobs show.

use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef, EffectGraphNode, SerializedParamValue};
use manifold_core::effects::{ParamConvert, ParamId, apply_card_reshape, invert_card_reshape};
use manifold_core::macro_bank::MacroCurve;
use manifold_core::{GraphTarget, LayerId, NodeId, project::Project};
use manifold_editing::command::Command;
use manifold_node_engine::param_binding::convert_param_value;
use manifold_node_engine::parameters::ParamValue;
use manifold_nodes_scene::node_graph::scene_vm::{ParamAddr, SceneObjectVm, SceneVm, ShapeOwner, ShapeWrite};
use manifold_nodes_scene::node_graph::viewport_gizmo::{
    GizmoDrag, GizmoMode, GizmoSelection, GizmoTarget, GizmoTargetId, GizmoTargetKind, HandleValues, HandleWrite, SelectionWrite,
    gizmo_target_for, gizmo_targets, pick_axis, pick_target, selection_writes, toggle_pick,
};
use manifold_ui::panels::scene_setup_panel::SceneSelection;
use manifold_ui::{GraphParamTarget, ScrubPhase, ScrubValue, ValueRef};

use crate::app_render::UnboundNodeParamDrag;
use crate::content_command::ContentCommand;
use crate::ui_bridge::DispatchCtx;
use crate::ui_bridge::scrub::{ResolvedScrub, begin_param_group, commit_param_group, dispatch_scrub, move_param_group};

/// Build the authored graph snapshot used by the viewport scene VM.
///
/// A direct scalar card binding is projected into its node literal using the
/// owner's base value; a scene modifier control's base value lands on its
/// host binding. Only editor geometry uses this projection; the viewport
/// runtime receives the original graph and the owner's effective manifest.
pub(crate) fn authored_def(project: &Project, target: &GraphTarget) -> Option<EffectGraphDef> {
    let resolved = crate::graph_target::resolve(project, target)?;
    let mut def = (*resolved).clone();
    project_authored_params(&mut def, project, target)?;
    Some(def)
}

pub(crate) fn project_authored_params(
    def: &mut EffectGraphDef,
    project: &Project,
    target: &GraphTarget,
) -> Option<()> {
    if !matches!(target, GraphTarget::Generator(_)) {
        return Some(());
    }
    let Some(owner) = project.graph_target_owner(target) else {
        return Some(());
    };
    fold_card_values(def, owner)
}

/// Fold `owner`'s card values into the node params their bindings drive.
pub(crate) fn fold_card_values(def: &mut EffectGraphDef, owner: &manifold_core::effects::PresetInstance) -> Option<()> {
    let Some(metadata) = def.preset_metadata.as_mut() else {
        return Some(());
    };

    for binding in &mut metadata.bindings {
        let (node_id, param) = match &binding.target {
            BindingTarget::Node { node_id, param } => (node_id, param),
            // A force's shape reads its card value off the host binding.
            BindingTarget::SceneModifier { .. } => {
                if owner.params.get(&binding.id).is_some() {
                    binding.default_value = owner.get_base_param(&binding.id);
                }
                continue;
            }
            BindingTarget::Composite { .. } => continue,
        };
        let Some(spec) = metadata.params.iter().find(|spec| spec.id == binding.id) else {
            continue;
        };
        if binding.convert == ParamConvert::Trigger || spec.is_trigger {
            continue;
        }

        if owner.params.get(&binding.id).is_none() {
            if binding.default_mirrors_node_param {
                continue;
            }
            let effective = apply_card_reshape(
                binding.default_value,
                spec.min,
                spec.max,
                spec.invert,
                spec.curve,
                binding.scale,
                binding.offset,
            );
            let value = scalar_to_serialized(binding.convert, effective)?;
            let node = find_node_by_stable_id_mut(&mut def.nodes, node_id)?;
            node.params.insert(param.clone(), value);
            continue;
        }

        let base = owner.get_base_param(&binding.id);
        if !base.is_finite() {
            return None;
        }
        binding.default_value = base;
        let effective = apply_card_reshape(
            base,
            spec.min,
            spec.max,
            spec.invert,
            spec.curve,
            binding.scale,
            binding.offset,
        );
        let value = scalar_to_serialized(binding.convert, effective)?;
        let node = find_node_by_stable_id_mut(&mut def.nodes, node_id)?;
        node.params.insert(param.clone(), value);
    }
    Some(())
}

fn scalar_to_serialized(convert: ParamConvert, value: f32) -> Option<SerializedParamValue> {
    if !value.is_finite() {
        return None;
    }
    match convert_param_value(convert, value) {
        ParamValue::Float(value) if value.is_finite() => {
            Some(SerializedParamValue::Float { value })
        }
        ParamValue::Bool(value) => Some(SerializedParamValue::Bool { value }),
        ParamValue::Enum(value) => Some(SerializedParamValue::Enum { value }),
        _ => None,
    }
}

fn find_node_by_stable_id_mut<'a>(
    nodes: &'a mut [EffectGraphNode],
    node_id: &NodeId,
) -> Option<&'a mut EffectGraphNode> {
    for node in nodes {
        if &node.node_id == node_id {
            return Some(node);
        }
        if let Some(group) = node.group.as_deref_mut()
            && let Some(found) = find_node_by_stable_id_mut(&mut group.nodes, node_id)
        {
            return Some(found);
        }
    }
    None
}

/// The knob a shape param already has.
#[derive(Debug, Clone, PartialEq)]
enum Knob {
    /// A card param, with the reshape from its value to the shape's.
    Card { id: ParamId, min: f32, max: f32, invert: bool, curve: MacroCurve, scale: f32, offset: f32 },
    /// An unbound graph node param.
    Node(ParamAddr),
}

impl Knob {
    /// The knob value that places the shape param at `value`.
    fn card_value(&self, value: f32) -> Option<f32> {
        match self {
            Knob::Card { min, max, invert, curve, scale, offset, .. } => {
                invert_card_reshape(value, *min, *max, *invert, *curve, *scale, *offset)
            }
            Knob::Node(_) => Some(value),
        }
    }
}

/// One gizmo drag in progress, scrubbing the knobs of the params its
/// handle writes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GizmoGesture {
    layer: LayerId,
    drag: GizmoDrag,
    kind: GestureKind,
    moved: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum GestureKind {
    /// One shape's handle, through the knobs of the params it writes.
    Shape(Vec<Knob>),
    /// The selection's handle: every member param it can write, scrubbed
    /// together as one gesture and one undo entry.
    Selection(Box<SelectionGesture>),
}

/// The members of a selection drag and the knob behind each param they
/// can write, held as they were at the press.
#[derive(Debug, Clone, PartialEq)]
struct SelectionGesture {
    members: Vec<GizmoTarget>,
    params: Vec<MemberParam>,
    /// Card values at the press, in the group scrub's card order.
    card_base: Vec<f32>,
    /// Unbound node values at the press, in the group scrub's node order.
    node_base: Vec<f32>,
    cards: Vec<f32>,
    nodes: Vec<f32>,
    writes: Vec<SelectionWrite>,
}

#[derive(Debug, Clone, PartialEq)]
struct MemberParam {
    member: usize,
    id: String,
    knob: Knob,
    slot: Slot,
}

/// Where a member param sits in the group scrub.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Slot {
    Card(usize),
    Node(usize),
}

impl GizmoGesture {
    /// Resolve each param's knob and begin its scrub. `Err` carries why the
    /// handle cannot be dragged.
    pub(crate) fn begin(ctx: &mut DispatchCtx, layer: LayerId, drag: GizmoDrag) -> Result<Self, String> {
        let target = GraphTarget::Generator(layer.clone());
        let knobs = drag
            .handle()
            .write
            .params()
            .iter()
            .map(|param| knob(&ctx.project, &target, &param.write))
            .collect::<Option<Vec<_>>>()
            .ok_or("This handle's parameter has no control to drag")?;
        match (&drag.handle().write, knobs.as_slice()) {
            (HandleWrite::Direction { .. }, [Knob::Card { id: x, .. }, Knob::Card { id: y, .. }, Knob::Card { id: z, .. }]) => {
                if !begin_param_group(ctx, target, vec![x.clone(), y.clone(), z.clone()], Vec::new(), "Turn direction") {
                    return Err("This direction cannot be turned here".into());
                }
            }
            (HandleWrite::Linear { .. } | HandleWrite::Angle(_), [Knob::Card { id, .. }]) => {
                dispatch_scrub(&card_ref(&layer, id), &ScrubPhase::Begin, ctx);
                if !matches!(&ctx.scrub.active, Some(ResolvedScrub::Param { param_id, .. }) if param_id == id) {
                    return Err("This handle's control is locked".into());
                }
            }
            (HandleWrite::Linear { param, .. } | HandleWrite::Angle(param), [Knob::Node(addr)]) => {
                let drag = node_drag(ctx, &layer, addr, param.value).ok_or("Generator graph is unavailable")?;
                ctx.scrub.check_single_active_on_begin("gizmo-node-param");
                ctx.scrub.active = Some(ResolvedScrub::UnboundNodeParam(Box::new(drag)));
            }
            (HandleWrite::Linear { .. } | HandleWrite::Angle(_) | HandleWrite::Direction { .. }, _) => {
                return Err("A direction turns only through its card controls".into());
            }
            (HandleWrite::Selection { .. }, _) => return Err("The selection's handle moves through its members".into()),
        }
        Ok(Self { layer, drag, kind: GestureKind::Shape(knobs), moved: false })
    }

    /// Begin dragging the selection's handle over `members`: every param a
    /// member can write, through its knob, as one gesture. A wired or
    /// macro-locked param, and an unplaced member, stay put. `Err` carries
    /// why nothing can move.
    pub(crate) fn begin_selection(ctx: &mut DispatchCtx, layer: LayerId, drag: GizmoDrag, members: Vec<GizmoTarget>) -> Result<Self, String> {
        let target = GraphTarget::Generator(layer.clone());
        let (mut params, mut cards, mut nodes) = (Vec::new(), Vec::<ParamId>::new(), Vec::new());
        let (mut card_base, mut node_base) = (Vec::new(), Vec::new());
        for (member, shape) in members.iter().enumerate().filter(|(_, shape)| !shape.unplaced) {
            for param in shape.handles.iter().flat_map(|handle| handle.write.params()) {
                if param.wired || params.iter().any(|seen: &MemberParam| seen.member == member && seen.id == param.id) {
                    continue;
                }
                let Some(knob) = knob(&ctx.project, &target, &param.write) else { continue };
                let slot = match &knob {
                    Knob::Card { id, .. } => {
                        if crate::scene_modifier_edit::macro_parameter_lock_reason(&ctx.project, &target, id.as_ref()).is_some() {
                            continue;
                        }
                        match cards.iter().position(|card| card == id) {
                            Some(at) => Slot::Card(at),
                            None => {
                                let Some(base) = ctx.project.with_preset_graph(&target, |inst| inst.get_base_param(id.as_ref())) else {
                                    continue;
                                };
                                cards.push(id.clone());
                                card_base.push(base);
                                Slot::Card(cards.len() - 1)
                            }
                        }
                    }
                    Knob::Node(addr) => {
                        let Some(drag) = node_drag(ctx, &layer, addr, param.value) else { continue };
                        nodes.push(drag);
                        node_base.push(param.value);
                        Slot::Node(nodes.len() - 1)
                    }
                };
                params.push(MemberParam { member, id: param.id.clone(), knob, slot });
            }
        }
        if params.is_empty() {
            return Err("Nothing selected can move here".into());
        }
        let label = match drag.mode {
            GizmoMode::Move => "Move selection",
            GizmoMode::Rotate => "Turn selection",
            GizmoMode::Scale => "Scale selection",
        };
        if !begin_param_group(ctx, target, cards, nodes, label) {
            return Err("The selection cannot move here".into());
        }
        let selection = SelectionGesture {
            members,
            params,
            cards: card_base.clone(),
            nodes: node_base.clone(),
            card_base,
            node_base,
            writes: Vec::new(),
        };
        Ok(Self { layer, drag, kind: GestureKind::Selection(Box::new(selection)), moved: false })
    }

    /// Move the handle with the pointer at `cur` (render pixels of `view`):
    /// a live, non-undoable write through each knob.
    pub(crate) fn drag_to(&mut self, ctx: &mut DispatchCtx, view: &GizmoView, cur: (f32, f32)) {
        let Some(values) = self.drag.update(view, cur) else { return };
        let knobs = match (&mut self.kind, values) {
            (GestureKind::Shape(knobs), _) => knobs,
            (GestureKind::Selection(selection), HandleValues::One(amount)) => {
                selection.move_to(ctx, self.drag.mode, self.drag.axis, amount);
                self.moved = true;
                return;
            }
            (GestureKind::Selection(_), HandleValues::Three(_)) => return,
        };
        match (values, knobs.as_slice()) {
            (HandleValues::One(value), [knob @ Knob::Card { id, .. }]) => {
                if let Some(card) = knob.card_value(value) {
                    dispatch_scrub(&card_ref(&self.layer, id), &ScrubPhase::Move(ScrubValue::Scalar(card)), ctx);
                }
            }
            (HandleValues::One(value), [Knob::Node(_)]) => {
                let target = GraphTarget::Generator(self.layer.clone());
                let graph_absent = ctx.project.preset_instance(&target).is_none_or(|inst| inst.graph.is_none());
                let Some(ResolvedScrub::UnboundNodeParam(drag)) = ctx.scrub.active.as_mut() else { return };
                drag.set_current(SerializedParamValue::Float { value });
                let live = (**drag).clone();
                // The first write may materialise the graph override (D21).
                let touches = crate::content_command::touch::shapes_first_move(!self.moved && graph_absent);
                ctx.gesture_move(touches, move |project| {
                    live.live_command().execute(project);
                });
            }
            (HandleValues::Three(values), [x, y, z]) => {
                if let (Some(x), Some(y), Some(z)) = (x.card_value(values[0]), y.card_value(values[1]), z.card_value(values[2])) {
                    move_param_group(ctx, &[x, y, z], &[]);
                }
            }
            (HandleValues::One(_) | HandleValues::Three(_), _) => return,
        }
        self.moved = true;
    }

    /// Release: one undo entry for the whole drag.
    pub(crate) fn commit(self, ctx: &mut DispatchCtx) {
        let knobs = match &self.kind {
            GestureKind::Shape(knobs) => knobs.as_slice(),
            GestureKind::Selection(_) => return commit_param_group(ctx),
        };
        match knobs {
            [Knob::Card { id, .. }] => {
                dispatch_scrub(&card_ref(&self.layer, id), &ScrubPhase::Commit, ctx);
            }
            [Knob::Node(_)] => {
                if let Some(ResolvedScrub::UnboundNodeParam(drag)) = ctx.scrub.active.take()
                    && self.moved
                    && drag.moved()
                {
                    ContentCommand::send(ctx.content_tx, ContentCommand::Execute(Box::new(drag.into_command())));
                }
            }
            _ => commit_param_group(ctx),
        }
    }
}

impl SelectionGesture {
    /// Every member's params with the handle dragged by `amount`, each from
    /// its value at the press, written live through the group scrub.
    fn move_to(&mut self, ctx: &mut DispatchCtx, mode: GizmoMode, axis: manifold_nodes_scene::node_graph::viewport_gizmo::GizmoAxis, amount: f32) {
        selection_writes(&self.members, mode, axis, amount, &mut self.writes);
        self.cards.copy_from_slice(&self.card_base);
        self.nodes.copy_from_slice(&self.node_base);
        for write in &self.writes {
            let Some(param) = self.params.iter().find(|param| param.member == write.member && param.id == write.param.id) else { continue };
            match param.slot {
                Slot::Card(at) => {
                    if let Some(card) = param.knob.card_value(write.value) {
                        self.cards[at] = card;
                    }
                }
                Slot::Node(at) => self.nodes[at] = write.value,
            }
        }
        move_param_group(ctx, &self.cards, &self.nodes);
    }
}

fn card_ref(layer: &LayerId, id: &ParamId) -> ValueRef {
    ValueRef::Param(GraphParamTarget::GeneratorOf(layer.clone()), id.clone())
}

/// An unbound node param scrub on `layer`'s generator graph from its value
/// now (`shown` when the key is absent). `None` without a graph.
fn node_drag(ctx: &DispatchCtx, layer: &LayerId, addr: &ParamAddr, shown: f32) -> Option<UnboundNodeParamDrag> {
    let target = GraphTarget::Generator(layer.clone());
    let catalog = crate::ui_bridge::generator_catalog_default(&ctx.project, layer)?;
    let pre_drag_value = ctx
        .project
        .graph_for_target(&target, Some(&catalog))
        .and_then(|def| crate::editor_bridge::node_param_value(def, &addr.scope_path, addr.node_doc_id, &addr.param_id));
    let current = pre_drag_value.clone().unwrap_or(SerializedParamValue::Float { value: shown });
    Some(UnboundNodeParamDrag::new(target, addr.scope_path.clone(), addr.node_doc_id, addr.param_id.clone(), catalog, pre_drag_value, current))
}

pub(crate) use manifold_nodes_scene::node_graph::viewport_gizmo::GizmoView;

/// What a left press on a gizmo host did.
#[derive(Debug)]
pub(crate) enum GizmoPress {
    /// The selection's handle took the press: `Some` when its drag began.
    /// A locked handle, or one whose drag cannot begin, still takes the
    /// press, so it never falls through to what lies under it.
    Handle(Option<Box<GizmoGesture>>),
    /// No handle there: the selection after the pick, or `None` for a press
    /// on empty space. A pick has already selected its items in the Scene
    /// panel.
    Picked(Option<Vec<GizmoTargetId>>),
}

/// Where a press landed: the picture it landed on, the point in that
/// render's pixels, and whether Shift extends the selection.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PressAt<'a> {
    pub view: &'a GizmoView,
    pub point: (f32, f32),
    pub extend: bool,
}

/// One press on any gizmo host (REALTIME_3D_DESIGN.md D7a): the
/// selection's one handle set first, then a pick. With `at.extend` (Shift)
/// the pick adds its shape to the selection or takes it out. `scene` is the
/// host's `SceneVm` of `layer`'s authored graph and `selection` was built
/// over it.
pub(crate) fn press(
    ctx: &mut DispatchCtx,
    layer: &LayerId,
    scene: &SceneVm,
    selection: &GizmoSelection,
    mode: GizmoMode,
    at: PressAt<'_>,
) -> GizmoPress {
    let PressAt { view, point, extend } = at;
    if let Some(target) = selection.handles.clone()
        && let Some(axis) = pick_axis(mode, &target, view, point)
    {
        // D8: a locked handle draws gray and refuses the drag.
        if target.handle(mode, axis).is_none_or(|handle| handle.write.locked()) {
            return GizmoPress::Handle(None);
        }
        if selection.selected.len() > 1 {
            let Some(drag) = GizmoDrag::begin(target, mode, axis, view, point) else { return GizmoPress::Handle(None) };
            return GizmoPress::Handle(match GizmoGesture::begin_selection(ctx, layer.clone(), drag, selection.selected.clone()) {
                Ok(gesture) => Some(Box::new(gesture)),
                Err(message) => {
                    ContentCommand::send(ctx.content_tx, ContentCommand::GraphEditRejected(message));
                    None
                }
            });
        }
        let target = if target.unplaced {
            match place(ctx, layer, &target) {
                Some(target) => target,
                None => return GizmoPress::Handle(None),
            }
        } else {
            target
        };
        let Some(drag) = GizmoDrag::begin(target, mode, axis, view, point) else { return GizmoPress::Handle(None) };
        return GizmoPress::Handle(match GizmoGesture::begin(ctx, layer.clone(), drag) {
            Ok(gesture) => Some(Box::new(gesture)),
            Err(message) => {
                ContentCommand::send(ctx.content_tx, ContentCommand::GraphEditRejected(message));
                None
            }
        });
    }
    let Some(picked) = pick_target(scene, view, point) else { return GizmoPress::Picked(None) };
    let next = toggle_pick(&selection.ids(), picked, extend);
    select_in_panel(ctx, layer, scene, &next);
    GizmoPress::Picked(Some(next))
}

/// The Scene panel follows the gizmo selection `ids`: the items their
/// shapes belong to, the last shape's item the one whose card shows.
pub(crate) fn select_in_panel(ctx: &mut DispatchCtx, layer: &LayerId, scene: &SceneVm, ids: &[GizmoTargetId]) {
    let mut items: Vec<SceneSelection> = Vec::new();
    for item in ids.iter().filter_map(|id| gizmo_target_for(scene, id)).filter_map(|target| scene_selection(scene, &target)) {
        items.retain(|other| *other != item);
        items.push(item);
    }
    ctx.ui.scene_setup_panel.select_items(layer.clone(), items);
}

/// The shapes the Scene panel's selected `items` show: for each item, its
/// shapes in `picked`, else its first declared shape.
pub(crate) fn selected_targets(scene: &SceneVm, items: &[SceneSelection], picked: &[GizmoTargetId]) -> Vec<GizmoTarget> {
    let mut out = Vec::new();
    for item in items {
        let belongs = |target: &GizmoTarget| scene_selection(scene, target).as_ref() == Some(item);
        let before = out.len();
        out.extend(picked.iter().filter_map(|id| gizmo_target_for(scene, id)).filter(belongs));
        if out.len() == before {
            out.extend(gizmo_targets(scene).find(belongs));
        }
    }
    out
}

/// D8 entry state: the shape's `via` transform is unwired, so the first
/// drag creates the atom (one undo unit) and the target is read again over
/// it. Only a scene object row places its own transform.
fn place(ctx: &mut DispatchCtx, layer: &LayerId, target: &GizmoTarget) -> Option<GizmoTarget> {
    let ShapeOwner::Object(obj_id) = target.id.owner else { return None };
    let graph = GraphTarget::Generator(layer.clone());
    let catalog = crate::ui_bridge::generator_catalog_default(&ctx.project, layer)?;
    let mut cmd = manifold_editing::commands::graph::AddObjectTransformCommand::new(graph.clone(), Vec::new(), obj_id, (0.0, 0.0), catalog);
    ctx.project.executed_by(|project| {
        cmd.execute(project);
        cmd.created_node_id()?;
        Some((Box::new(cmd) as Box<dyn Command>, ()))
    })?;
    let scene = SceneVm::from_def(&authored_def(&ctx.project, &graph)?)?;
    gizmo_target_for(&scene, &target.id)
}

/// The Scene panel item a target belongs to, by its kind: a force selects
/// itself; a placed Inflow, Drain or Collider selects its own row; an
/// object, a liquid domain, a water's own fluid source and a swarm box select
/// their object row.
pub(crate) fn scene_selection(scene: &SceneVm, target: &GizmoTarget) -> Option<SceneSelection> {
    if let ShapeOwner::RoleSource { node, .. } = &target.id.owner
        && scene.objects.iter().any(|object| {
            matches!(object, SceneObjectVm::Known(row) if row.placed_sources.iter().any(|source| &source.node == node))
        })
    {
        return Some(SceneSelection::Source(node.clone()));
    }
    match target.kind {
        GizmoTargetKind::Force => match &target.id.owner {
            ShapeOwner::Force(node) => Some(SceneSelection::Force(node.clone())),
            ShapeOwner::Object(_) | ShapeOwner::LiquidDomain(_) | ShapeOwner::RoleSource { .. } | ShapeOwner::Swarm(_) => None,
        },
        GizmoTargetKind::Object
        | GizmoTargetKind::FluidDomain
        | GizmoTargetKind::InitialFill
        | GizmoTargetKind::Inflow
        | GizmoTargetKind::Drain
        | GizmoTargetKind::Collider
        | GizmoTargetKind::Swarm => target.row.map(SceneSelection::Object),
    }
}

/// The knob behind `write`: a host card, a card bound to the node param, or
/// the node param itself.
fn knob(project: &Project, target: &GraphTarget, write: &ShapeWrite) -> Option<Knob> {
    let owner = project.graph_target_owner(target)?;
    let card = |id: String, scale: f32, offset: f32| {
        let spec = &owner.params.get(&id)?.spec;
        Some(Knob::Card { id: ParamId::from(id), min: spec.min, max: spec.max, invert: spec.invert, curve: spec.curve, scale, offset })
    };
    match write {
        ShapeWrite::Card { id, scale, offset } => card(id.clone(), *scale, *offset),
        // The host places the node before a drag begins.
        ShapeWrite::Unplaced => None,
        ShapeWrite::Node(addr) => {
            let def = crate::graph_target::resolve(project, target)?;
            match crate::ui_bridge::project::scene_binding_id(&def, &addr.scope_path, addr.node_doc_id, &addr.param_id) {
                Some(id) => {
                    let binding = def.preset_metadata.as_ref()?.bindings.iter().find(|binding| binding.id == id)?;
                    card(id.clone(), binding.scale, binding.offset)
                }
                None => Some(Knob::Node(addr.clone())),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::app::SelectionState;
    use crate::ui_root::UIRoot;
    use manifold_core::PresetTypeId;
    use manifold_core::effect_graph_def::{BindingDef, EffectGraphWire, ParamSpecDef};
    use manifold_core::types::LayerType;
    use manifold_editing::service::EditingService;
    use manifold_node_engine::scene::camera::Camera;
    use manifold_nodes_scene::node_graph::scene_vm::{SceneVm, ShapeOwner};
    use manifold_nodes_scene::node_graph::viewport_gizmo::{
        GizmoAxis, GizmoMode, GizmoTarget, GizmoTargetKind, gizmo_targets,
    };

    const GPU_FLIP_WATER: &str =
        manifold_nodes::testkit::assets::ASSETS_GENERATOR_PRESETS_WATERDAMBREAKGPUFLIP_JSON;
    const W: u32 = 640;
    const H: u32 = 480;

    /// The UI side of a dispatch, as `rack_controls`' tests build it.
    struct Rig {
        tx: crossbeam_channel::Sender<ContentCommand>,
        rx: crossbeam_channel::Receiver<ContentCommand>,
        ui: UIRoot,
        selection: SelectionState,
        active_layer: Option<LayerId>,
        prefs: crate::user_prefs::UserPrefs,
        scrub: crate::ui_bridge::ScrubState,
        content: crate::content_state::ContentState,
    }

    impl Rig {
        fn new(layer: &LayerId) -> Self {
            let (tx, rx) = crossbeam_channel::unbounded();
            Self {
                tx,
                rx,
                ui: UIRoot::new(),
                selection: manifold_ui::UIState::new(),
                active_layer: Some(layer.clone()),
                prefs: crate::user_prefs::UserPrefs::in_memory(),
                scrub: crate::ui_bridge::ScrubState::default(),
                content: crate::content_state::ContentState::default(),
            }
        }

        /// Run `f` in the dispatch context a host press runs in.
        fn with_ctx<R>(&mut self, project: &mut Project, f: impl FnOnce(&mut DispatchCtx) -> R) -> R {
            let mut edits = crate::gesture_preview::UiEdits::default();
            let mut ctx = DispatchCtx {
                project: crate::ui_bridge::ProjectRef::new(project, &mut edits, &self.tx),
                content_tx: &self.tx,
                content_state: &self.content,
                ui: &mut self.ui,
                selection: &mut self.selection,
                active_layer: &mut self.active_layer,
                user_prefs: &mut self.prefs,
                editor_target: None,
                scrub: &mut self.scrub,
                dialogs: Default::default(),
            };
            f(&mut ctx)
        }

        /// One full gesture: press `target`'s handle at `from`, drag to
        /// `to`, release.
        fn drag(&mut self, project: &mut Project, layer: &LayerId, target: &GizmoTarget, mode: GizmoMode, axis: GizmoAxis, from: (f32, f32), to: (f32, f32)) {
            self.with_ctx(project, |ctx| {
                let view = GizmoView::new(camera(), W, H);
                let drag = GizmoDrag::begin(target.clone(), mode, axis, &view, from).expect("handle is draggable");
                let mut gesture = GizmoGesture::begin(ctx, layer.clone(), drag).expect("gesture begins");
                gesture.drag_to(ctx, &view, to);
                gesture.commit(ctx);
                assert!(ctx.scrub.active.is_none(), "release ends the scrub");
            });
        }

        /// Apply what reached the content thread. Returns the undoable
        /// commands that landed.
        fn apply(&self, service: &mut EditingService, project: &mut Project) -> usize {
            let mut landed = 0;
            for cmd in self.rx.try_iter() {
                match cmd {
                    ContentCommand::MutateProject { f, .. } => f(project),
                    ContentCommand::Execute(cmd) | ContentCommand::ExecuteOnContent(cmd) => {
                        service.execute(cmd, &mut *project);
                        landed += 1;
                    }
                    _ => {}
                }
            }
            landed
        }
    }

    fn camera() -> Camera {
        Camera::look_at([0.0, 2.0, 14.0], [0.0, 2.0, 0.0], [0.0, 1.0, 0.0], 0.9, 0.1, 100.0)
    }

    fn px(point: [f32; 3]) -> (f32, f32) {
        let p = camera().project_to_pixel(point, W, H).unwrap();
        (p.px, p.py)
    }

    fn plus(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
    }

    fn scene(project: &Project, layer: &LayerId) -> SceneVm {
        let def = authored_def(project, &GraphTarget::Generator(layer.clone())).unwrap();
        SceneVm::from_def(&def).unwrap()
    }

    fn target(project: &Project, layer: &LayerId, kind: GizmoTargetKind) -> GizmoTarget {
        gizmo_targets(&scene(project, layer))
            .find(|target| target.kind == kind && target.id.shape == 0)
            .unwrap_or_else(|| panic!("a {kind:?} target"))
    }

    /// The value the scene places `param` of `target`'s shape at.
    fn shape_value(project: &Project, layer: &LayerId, target: &GizmoTarget, param: &str) -> f32 {
        let scene = scene(project, layer);
        let shape = scene.shapes.iter().find(|shape| shape.owner == target.id.owner && shape.index == target.id.shape).unwrap();
        shape.params.iter().find(|p| p.id == param).unwrap().value
    }

    fn base(project: &Project, layer: &LayerId, id: &str) -> f32 {
        project.graph_target_owner(&GraphTarget::Generator(layer.clone())).unwrap().get_base_param(id)
    }

    /// A press at `point` with the shapes `selected` selected, as a host
    /// presses; `extend` is Shift.
    fn tap(ctx: &mut DispatchCtx, layer: &LayerId, scene: &SceneVm, selected: &[GizmoTargetId], extend: bool, view: &GizmoView, point: (f32, f32)) -> GizmoPress {
        let selection = GizmoSelection::of_ids(scene, selected);
        press(ctx, layer, scene, &selection, GizmoMode::Move, PressAt { view, point, extend })
    }

    /// The water's domain box and its fill column.
    fn domain_and_column(project: &Project, layer: &LayerId) -> (GizmoTarget, GizmoTarget) {
        let scene = scene(project, layer);
        let domain = target(project, layer, GizmoTargetKind::FluidDomain);
        let column = gizmo_targets(&scene)
            .find(|t| t.kind == GizmoTargetKind::InitialFill)
            .expect("the GPU FLIP family places an Initial Fill");
        (domain, column)
    }

    /// BUG-6u36g, as Peter hit it: with Water 1's fill column picked, a
    /// click on the domain box's edge moves the handles to the domain. The
    /// box was picked by its centre alone, so its edges took no click.
    #[test]
    fn the_domain_box_is_picked_by_its_edge_after_the_fill_column() {
        let (mut project, layer) = fluid_project(false);
        let (domain, column) = domain_and_column(&project, &layer);
        let scene = scene(&project, &layer);
        let view = GizmoView::new(camera(), W, H);
        let mut rig = Rig::new(&layer);
        let picked = rig.with_ctx(&mut project, |ctx| tap(ctx, &layer, &scene, &[], false, &view, px(column.origin)));
        assert!(matches!(&picked, GizmoPress::Picked(Some(ids)) if *ids == [column.id.clone()]), "the column is picked first: {picked:?}");
        // The domain's near left edge, halfway up: far from either centre,
        // so a pick by centres alone finds nothing here.
        let edge = px([-2.0, 2.0, 2.0]);
        for centre in [domain.origin, column.origin] {
            let (x, y) = px(centre);
            let far = (x - edge.0).hypot(y - edge.1) > manifold_nodes_scene::node_graph::viewport_gizmo::PICK_RADIUS_PT;
            assert!(far, "the edge is clear of the centre at {centre:?}");
        }
        let picked = rig.with_ctx(&mut project, |ctx| tap(ctx, &layer, &scene, std::slice::from_ref(&column.id), false, &view, edge));
        assert!(matches!(&picked, GizmoPress::Picked(Some(ids)) if *ids == [domain.id.clone()]), "the domain box takes the click: {picked:?}");
        assert_eq!(
            selected_targets(&scene, &[SceneSelection::Object(domain.row.unwrap())], std::slice::from_ref(&domain.id)).pop().map(|t| t.id),
            Some(domain.id),
            "the domain's handles show"
        );
    }

    /// Shift-click adds a shape to the selection, a second Shift-click on
    /// it takes it out, and a plain click selects one.
    #[test]
    fn shift_click_adds_and_removes_a_shape() {
        let (mut project, layer) = fluid_project(false);
        let (domain, column) = domain_and_column(&project, &layer);
        let scene = scene(&project, &layer);
        let view = GizmoView::new(camera(), W, H);
        let mut rig = Rig::new(&layer);
        let mut tap_at = |selected: &[GizmoTargetId], extend: bool, point: (f32, f32)| match rig.with_ctx(&mut project, |ctx| tap(ctx, &layer, &scene, selected, extend, &view, point)) {
            GizmoPress::Picked(Some(ids)) => ids,
            other => panic!("a pick, got {other:?}"),
        };
        let edge = px([-2.0, 2.0, 2.0]);
        let both = tap_at(std::slice::from_ref(&domain.id), true, px(column.origin));
        assert_eq!(both, [domain.id.clone(), column.id.clone()], "Shift adds");
        let one = tap_at(&both, true, edge);
        assert_eq!(one, std::slice::from_ref(&column.id), "Shift on a selected shape takes it out");
        assert_eq!(tap_at(&both, false, edge), std::slice::from_ref(&domain.id), "a plain click selects one");
    }

    /// One handle set moves the whole selection: a bound card and an
    /// unbound node param, one undo entry for both, and undo puts both back.
    #[test]
    fn the_selection_moves_as_one_undo_step() {
        let (mut local, layer) = fluid_project(true);
        let mut content = local.clone();
        let (domain, column) = domain_and_column(&local, &layer);
        let scene = scene(&local, &layer);
        let view = GizmoView::new(camera(), W, H);
        let selection = GizmoSelection::of_ids(&scene, &[domain.id.clone(), column.id.clone()]);
        let group = selection.handles.clone().expect("one handle set for the selection");
        let grip = group.grip(GizmoMode::Move, GizmoAxis::X, &view).expect("the selection's X handle");
        let column_x = shape_value(&local, &layer, &column, "pos_x");
        let mut rig = Rig::new(&layer);
        rig.with_ctx(&mut local, |ctx| {
            let at = PressAt { view: &view, point: px(grip), extend: false };
            let GizmoPress::Handle(Some(mut gesture)) = press(ctx, &layer, &scene, &selection, GizmoMode::Move, at) else {
                panic!("the selection's handle takes the press")
            };
            gesture.drag_to(ctx, &view, px(plus(grip, [0.5, 0.0, 0.0])));
            gesture.drag_to(ctx, &view, px(plus(grip, [1.0, 0.0, 0.0])));
            gesture.commit(ctx);
            assert!(ctx.scrub.active.is_none(), "release ends the scrub");
        });
        let mut service = EditingService::new();
        assert_eq!(rig.apply(&mut service, &mut content), 1, "one undo entry for the whole selection");
        for project in [&local, &content] {
            assert!((base(project, &layer, "domain_x") - 2.5).abs() < 1e-3, "the domain's card moved one unit");
            assert!((shape_value(project, &layer, &column, "pos_x") - column_x - 1.0).abs() < 1e-3, "the column moved one unit");
        }
        assert!(service.undo(&mut content));
        assert_eq!(base(&content, &layer, "domain_x"), 1.5);
        assert!((shape_value(&content, &layer, &column, "pos_x") - column_x).abs() < 1e-6, "one undo puts both back");
    }

    /// The GPU FLIP water as it ships: no transform on the domain port, so
    /// Domain Size alone sizes the solver's cube. `bound` puts Domain Size
    /// on a card, as the inserted water's Simulation group does.
    fn cube_project(bound: bool) -> (Project, LayerId) {
        let mut project = Project::default();
        let index = project.timeline.add_layer(
            "Fluid",
            LayerType::Generator,
            PresetTypeId::from_string("WaterDamBreakGpuFlip".to_string()),
        );
        let layer_id = project.timeline.layers[index].layer_id.clone();
        let mut def: EffectGraphDef = serde_json::from_str(GPU_FLIP_WATER).unwrap();
        if bound {
            let domain = def
                .nodes
                .iter()
                .filter_map(|node| node.group.as_ref())
                .flat_map(|group| group.nodes.iter())
                .find(|node| node.type_id == manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID)
                .expect("GPU FLIP domain")
                .node_id
                .clone();
            let metadata = def.preset_metadata.as_mut().unwrap();
            metadata.params.push(ParamSpecDef {
                id: "domain_size_card".into(),
                name: "Domain Size".into(),
                min: 0.5,
                max: 20.0,
                default_value: 4.0,
                ..ParamSpecDef::default()
            });
            metadata.bindings.push(BindingDef {
                id: "domain_size_card".into(),
                label: "Domain Size".into(),
                default_value: 4.0,
                target: BindingTarget::Node { node_id: domain, param: "domain_size".into() },
                convert: ParamConvert::Float,
                user_added: false,
                scale: 1.0,
                offset: 0.0,
                default_mirrors_node_param: false,
            });
        }
        let generator = project.timeline.layers[index].gen_params_mut().unwrap();
        generator.graph = Some(def);
        generator.refresh_manifest_from_graph();
        (project, layer_id)
    }

    /// BUG-6u36g, as Peter hit it on an inserted water: the domain cube had
    /// no target at all. It takes a click on its own edge, and its one
    /// uniform scale handle writes Domain Size as one undo step.
    #[test]
    fn an_unwired_domain_cube_is_picked_and_scales_its_domain_size() {
        for bound in [false, true] {
            unwired_domain_cube(bound);
        }
    }

    fn unwired_domain_cube(bound: bool) {
        let (mut local, layer) = cube_project(bound);
        let mut content = local.clone();
        let domain = target(&local, &layer, GizmoTargetKind::FluidDomain);
        let size = shape_value(&local, &layer, &domain, "scale_x");
        assert_eq!(domain.origin, [0.0, size * 0.5, 0.0], "the cube sits on the floor at the origin");
        assert!(domain.handle(GizmoMode::Move, GizmoAxis::X).is_some_and(|h| h.write.locked()), "nothing positions the cube");
        assert!(domain.handle(GizmoMode::Scale, GizmoAxis::X).is_none(), "a cube has no side of its own");
        let Some(ShapeWrite::Node(addr)) = domain.handle(GizmoMode::Scale, GizmoAxis::Uniform).map(|h| h.write.params()[0].write.clone())
        else {
            panic!("the cube scales through a node param")
        };
        assert_eq!(addr.param_id, "domain_size");

        let scene = scene(&local, &layer);
        let view = GizmoView::new(camera(), W, H);
        let mut rig = Rig::new(&layer);
        let edge = px([0.0, size, size * 0.5]);
        let picked = rig.with_ctx(&mut local, |ctx| tap(ctx, &layer, &scene, &[], false, &view, edge));
        assert!(matches!(&picked, GizmoPress::Picked(Some(ids)) if *ids == [domain.id.clone()]), "the cube's top edge picks it: {picked:?}");

        // The uniform handle runs along the diagonal.
        let along = |d: f32| px(plus(domain.origin, GizmoAxis::Uniform.unit().map(|c| c * d)));
        rig.drag(&mut local, &layer, &domain, GizmoMode::Scale, GizmoAxis::Uniform, along(1.0), along(1.25));
        let mut service = EditingService::new();
        assert_eq!(rig.apply(&mut service, &mut content), 1, "one undo entry for the drag");
        for project in [&local, &content] {
            let grown = shape_value(project, &layer, &domain, "scale_x");
            assert!((grown - size * 1.25).abs() < 1e-3, "bound {bound}: a quarter farther out scales Domain Size a quarter up, got {grown}");
            if bound {
                assert!((base(project, &layer, "domain_size_card") - size * 1.25).abs() < 1e-3, "the slider's card moves with it");
            }
        }
        assert!(service.undo(&mut content));
        assert_eq!(shape_value(&content, &layer, &domain, "scale_x"), size);
        if bound {
            assert_eq!(base(&content, &layer, "domain_size_card"), size);
        }
    }

    fn fluid_project(bound: bool) -> (Project, LayerId) {
        let mut project = Project::default();
        let index = project.timeline.add_layer(
            "Fluid",
            LayerType::Generator,
            PresetTypeId::from_string("WaterDamBreakGpuFlip".to_string()),
        );
        let layer_id = project.timeline.layers[index].layer_id.clone();
        let mut def: EffectGraphDef = serde_json::from_str(GPU_FLIP_WATER).unwrap();
        let family = def
            .nodes
            .iter_mut()
            .find(|node| node.group.as_deref().and_then(manifold_core::scene_object_model::water_family).is_some())
            .expect("GPU FLIP family");
        let group = family.group.as_mut().expect("GPU FLIP family body");
        let domain = group
            .nodes
            .iter()
            .find(|node| node.type_id == manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID)
            .expect("GPU FLIP domain")
            .id;
        let transform_id = group.nodes.iter().map(|node| node.id).max().unwrap_or(0) + 1;
        group.nodes.push(EffectGraphNode {
            id: transform_id,
            node_id: NodeId::new("domain_transform"),
            type_id: "node.transform_3d".into(),
            handle: Some("Domain".into()),
            params: BTreeMap::from([
                ("pos_y".into(), SerializedParamValue::Float { value: 2.0 }),
                ("scale_x".into(), SerializedParamValue::Float { value: 4.0 }),
                ("scale_y".into(), SerializedParamValue::Float { value: 4.0 }),
                ("scale_z".into(), SerializedParamValue::Float { value: 4.0 }),
            ]),
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: Default::default(),
            output_canvas_scales: Default::default(),
            group: None,
        });
        group.wires.push(EffectGraphWire {
            from_node: transform_id,
            from_port: "transform".into(),
            to_node: domain,
            to_port: "domain".into(),
        });
        if bound {
            // The width card's range is wider than the solvers accept, so
            // only the domain's declared limit can stop its drag.
            for (id, name, param, min, max, default_value) in
                [("domain_x", "Domain X", "pos_x", -10.0, 10.0, 0.0), ("domain_width", "Domain Width", "scale_x", 0.0, 50.0, 4.0)]
            {
                let metadata = def.preset_metadata.as_mut().unwrap();
                metadata.params.push(ParamSpecDef {
                    id: id.into(),
                    name: name.into(),
                    min,
                    max,
                    default_value,
                    ..ParamSpecDef::default()
                });
                metadata.bindings.push(BindingDef {
                    id: id.into(),
                    label: name.into(),
                    default_value,
                    target: BindingTarget::Node { node_id: NodeId::new("domain_transform"), param: param.into() },
                    convert: ParamConvert::Float,
                    user_added: false,
                    scale: 1.0,
                    offset: 0.0,
                    default_mirrors_node_param: false,
                });
            }
        }
        let layer = &mut project.timeline.layers[index];
        let generator = layer.gen_params_mut().unwrap();
        generator.graph = Some(def);
        generator.refresh_manifest_from_graph();
        if bound {
            generator.set_base_param("domain_x", 1.5);
        }
        (project, layer_id)
    }

    #[test]
    fn authored_projection_folds_card_values_into_the_graph() {
        let (project, layer_id) = fluid_project(true);
        let graph = GraphTarget::Generator(layer_id.clone());
        let before = crate::graph_target::resolve(&project, &graph).map(|graph| (*graph).clone()).unwrap();
        let mut projected = authored_def(&project, &graph).unwrap();
        let domain = find_node_by_stable_id_mut(&mut projected.nodes, &NodeId::new("domain_transform")).unwrap();
        assert_eq!(domain.params.get("pos_x"), Some(&SerializedParamValue::Float { value: 1.5 }));
        let binding = projected.preset_metadata.as_ref().unwrap().bindings.iter().find(|b| b.id == "domain_x").unwrap();
        assert_eq!(binding.default_value, 1.5);
        assert_eq!(&*crate::graph_target::resolve(&project, &graph).unwrap(), &before, "the project is untouched");
        assert_eq!(target(&project, &layer_id, GizmoTargetKind::FluidDomain).origin[0], 1.5);
    }

    #[test]
    fn bound_domain_drag_scrubs_its_card_and_undoes_in_one_step() {
        let (mut local, layer) = fluid_project(true);
        let mut content = local.clone();
        let domain = target(&local, &layer, GizmoTargetKind::FluidDomain);
        let mut rig = Rig::new(&layer);
        rig.drag(&mut local, &layer, &domain, GizmoMode::Move, GizmoAxis::X, px(domain.origin), px(plus(domain.origin, [1.0, 0.0, 0.0])));
        let mut service = EditingService::new();
        assert_eq!(rig.apply(&mut service, &mut content), 1, "one undo entry for the drag");
        for project in [&local, &content] {
            assert!((base(project, &layer, "domain_x") - 2.5).abs() < 1e-3, "the card moved one unit");
        }
        assert!(service.undo(&mut content));
        assert_eq!(base(&content, &layer, "domain_x"), 1.5);
    }

    #[test]
    fn unbound_domain_drag_writes_its_node_param_once() {
        let (mut local, layer) = fluid_project(false);
        let mut content = local.clone();
        let domain = target(&local, &layer, GizmoTargetKind::FluidDomain);
        let Some(ShapeWrite::Node(addr)) = domain.handle(GizmoMode::Scale, GizmoAxis::Y).map(|h| h.write.params()[0].write.clone()) else {
            panic!("an unbound node write")
        };
        let mut rig = Rig::new(&layer);
        rig.drag(&mut local, &layer, &domain, GizmoMode::Scale, GizmoAxis::Y, px(plus(domain.origin, [0.0, 1.0, 0.0])), px(plus(domain.origin, [0.0, 1.25, 0.0])));
        let mut service = EditingService::new();
        assert_eq!(rig.apply(&mut service, &mut content), 1, "one undo entry for the drag");
        let value = |project: &Project| {
            let graph = GraphTarget::Generator(layer.clone());
            let def = crate::graph_target::resolve(project, &graph).unwrap();
            crate::editor_bridge::node_param_value(&def, &addr.scope_path, addr.node_doc_id, &addr.param_id)
        };
        for project in [&local, &content] {
            let Some(SerializedParamValue::Float { value: v }) = value(project) else { panic!("scale_y is a float") };
            assert!((v - 5.0).abs() < 1e-3, "a quarter farther from the centre scales a quarter up, got {v}");
        }
        assert!(service.undo(&mut content));
        assert_eq!(value(&content), Some(SerializedParamValue::Float { value: 4.0 }));
    }

    /// The solvers refuse a domain side outside their range, so a scale
    /// drag stops at it through either knob path.
    #[test]
    fn domain_scale_drag_stops_at_the_solver_range() {
        let (min, max) = manifold_core::fluid_domain::DOMAIN_EXTENT_METRES;
        for bound in [false, true] {
            for (reach, stop) in [(-10.0, min), (30.0, max)] {
                let (mut project, layer) = fluid_project(bound);
                let domain = target(&project, &layer, GizmoTargetKind::FluidDomain);
                let mut rig = Rig::new(&layer);
                let to = px(plus(domain.origin, [reach, 0.0, 0.0]));
                rig.drag(&mut project, &layer, &domain, GizmoMode::Scale, GizmoAxis::X, px(plus(domain.origin, [1.0, 0.0, 0.0])), to);
                let size = shape_value(&project, &layer, &domain, "scale_x");
                assert!((size - stop).abs() < 1e-4, "bound {bound}: a drag by {reach} stops at {stop}, got {size}");
                if bound {
                    assert!((base(&project, &layer, "domain_width") - stop).abs() < 1e-4, "the card stops with it");
                }
            }
        }
    }

    /// A force saved before shapes were declared still gets its gizmo: the
    /// declaration is read from the preset registry, not the saved copy.
    #[test]
    fn a_force_saved_before_shape_declarations_gets_its_gizmo() {
        let mut def: EffectGraphDef =
            serde_json::from_str(include_str!("../../manifold-io/tests/fixtures/water_layer_graph_v1160.json")).unwrap();
        assert!(!def.scene_modifiers.is_empty(), "the saved layer has forces");
        assert!(
            def.scene_modifiers.iter().all(|m| m.graph.preset_metadata.as_ref().is_some_and(|p| p.shape.is_none())),
            "the saved copies predate the declaration"
        );
        // Give each force its host cards, as adding it does.
        for id in def.scene_modifiers.iter().map(|m| m.id.clone()).collect::<Vec<_>>() {
            def = manifold_core::scene_modifier_edit::reconcile_scene_modifier_parameters(&def, &id).unwrap().graph;
        }
        let scene = SceneVm::from_def(&def).expect("the saved layer builds a scene");
        for modifier in &def.scene_modifiers {
            let owner = ShapeOwner::Force(modifier.id.clone());
            assert!(scene.shapes.iter().any(|shape| shape.owner == owner), "{} has a gizmo shape", modifier.id);
        }
    }

    /// A force card locked by a calibrated modifier draws locked, and the
    /// press refuses it: draw and press read one predicate.
    #[test]
    fn a_macro_locked_force_handle_draws_locked_and_refuses_the_press() {
        let mut project = crate::ui_bridge::force_project("RadialForce", &["force_a"]);
        let layer = LayerId::new("scene");
        let generator = project.timeline.layers[0].gen_params_mut().unwrap();
        let graph = generator.graph.as_mut().unwrap();
        let metadata = graph.preset_metadata.as_mut().unwrap();
        let card = metadata
            .bindings
            .iter()
            .find(|b| matches!(&b.target, BindingTarget::SceneModifier { param_id, .. } if param_id == "center_x"))
            .expect("the force's center X card")
            .clone();
        let source = NodeId::new("scan_render");
        metadata.bindings.push(BindingDef {
            target: BindingTarget::Node { node_id: source.clone(), param: "calibration_probe".into() },
            ..card.clone()
        });
        let scan = manifold_core::scene_modifier_preset::SceneNodeRef { scope: vec![], node: source };
        graph.scene_modifiers[0].mesh_frames.push(manifold_core::scene_modifier_preset::SceneMeshReferenceFrame {
            target: scan.clone(),
            source: scan,
            source_definition_hash: String::new(),
            source_offset: [0.0; 3],
            scene_radius: 1.0,
        });
        assert!(
            crate::scene_modifier_edit::macro_parameter_lock_reason(&project, &GraphTarget::Generator(layer.clone()), &card.id).is_some(),
            "the press side refuses the card"
        );

        let force = target(&project, &layer, GizmoTargetKind::Force);
        assert!(force.handle(GizmoMode::Move, GizmoAxis::X).unwrap().write.locked(), "the locked card's handle draws locked");
        assert!(!force.handle(GizmoMode::Move, GizmoAxis::Y).unwrap().write.locked(), "an unlocked card's handle does not");
        let view = GizmoView::new(camera(), W, H);
        let scene = scene(&project, &layer);
        let grip = force.grip(GizmoMode::Move, GizmoAxis::X, &view).unwrap();
        let mut rig = Rig::new(&layer);
        let pressed = rig.with_ctx(&mut project, |ctx| tap(ctx, &layer, &scene, std::slice::from_ref(&force.id), false, &view, px(grip)));
        assert!(matches!(pressed, GizmoPress::Handle(None)), "the locked handle takes the press and refuses it: {pressed:?}");
        assert!(rig.rx.try_iter().next().is_none(), "nothing reaches the content thread");
    }

    #[test]
    fn radial_force_drag_writes_its_center_card() {
        let mut local = crate::ui_bridge::force_project("RadialForce", &["force_a"]);
        let layer = LayerId::new("scene");
        let mut content = local.clone();
        let force = target(&local, &layer, GizmoTargetKind::Force);
        assert!(matches!(force.id.owner, ShapeOwner::Force(_)));
        let before = shape_value(&local, &layer, &force, "center_x");
        let mut rig = Rig::new(&layer);
        rig.drag(&mut local, &layer, &force, GizmoMode::Move, GizmoAxis::X, px(force.origin), px(plus(force.origin, [1.0, 0.0, 0.0])));
        let mut service = EditingService::new();
        assert_eq!(rig.apply(&mut service, &mut content), 1);
        for project in [&local, &content] {
            let after = shape_value(project, &layer, &force, "center_x");
            assert!((after - before - 1.0).abs() < 1e-3, "center moved one unit: {before} -> {after}");
        }
        assert!(service.undo(&mut content));
        assert_eq!(shape_value(&content, &layer, &force, "center_x"), before);
    }

    #[test]
    fn vortex_axis_turns_as_one_undo_entry() {
        let mut local = crate::ui_bridge::force_project("VortexForce", &["force_a"]);
        let layer = LayerId::new("scene");
        let mut content = local.clone();
        let force = target(&local, &layer, GizmoTargetKind::Force);
        let axis = |project: &Project| ["axis_x", "axis_y", "axis_z"].map(|id| shape_value(project, &layer, &force, id));
        let before = axis(&local);
        let o = px(force.origin);
        let mut rig = Rig::new(&layer);
        // A quarter turn about the view axis, swept on screen.
        rig.drag(&mut local, &layer, &force, GizmoMode::Rotate, GizmoAxis::Z, (o.0 + 100.0, o.1), (o.0, o.1 - 100.0));
        let mut service = EditingService::new();
        assert_eq!(rig.apply(&mut service, &mut content), 1, "three params, one undo entry");
        let length = |v: [f32; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        for project in [&local, &content] {
            let after = axis(project);
            assert_ne!(after, before, "the axis turned");
            assert!((length(after) - length(before)).abs() < 1e-3, "{before:?} -> {after:?}");
        }
        assert!(service.undo(&mut content));
        assert_eq!(axis(&content), before);
    }

    /// Both hosts press through `press`: a click on nothing selected picks
    /// the force under it, and the next press on its move handle drags the
    /// card as the knob does, one undo entry.
    #[test]
    fn a_host_press_picks_then_drags_through_the_knob() {
        let mut local = crate::ui_bridge::force_project("RadialForce", &["force_a"]);
        let layer = LayerId::new("scene");
        let mut content = local.clone();
        let force = target(&local, &layer, GizmoTargetKind::Force);
        let view = GizmoView::new(camera(), W, H);
        let before = shape_value(&local, &layer, &force, "center_x");
        let mut rig = Rig::new(&layer);
        let scene = scene(&local, &layer);
        let picked = rig.with_ctx(&mut local, |ctx| tap(ctx, &layer, &scene, &[], false, &view, px(force.origin)));
        let GizmoPress::Picked(Some(mut ids)) = picked else { panic!("the force is picked, got {picked:?}") };
        let id = ids.pop().unwrap();
        assert_eq!(id, force.id);
        // REALTIME_3D D9: a pick is UI state only; nothing reaches the
        // content thread, so the output render cannot change.
        assert!(rig.rx.try_iter().next().is_none(), "a pick sends no content command");

        let grip = force.grip(GizmoMode::Move, GizmoAxis::X, &view).unwrap();
        rig.with_ctx(&mut local, |ctx| {
            let GizmoPress::Handle(Some(mut gesture)) = tap(ctx, &layer, &scene, std::slice::from_ref(&id), false, &view, px(grip)) else {
                panic!("the move handle takes the press")
            };
            gesture.drag_to(ctx, &view, px(plus(grip, [1.0, 0.0, 0.0])));
            gesture.commit(ctx);
        });
        let mut service = EditingService::new();
        assert_eq!(rig.apply(&mut service, &mut content), 1, "one undo entry for the drag");
        let after = shape_value(&local, &layer, &force, "center_x");
        assert!((after - before - 1.0).abs() < 1e-3, "center moved one unit: {before} -> {after}");
    }

    /// GizmoTargetKind routes a pick to its Scene panel item: a force to
    /// itself, a liquid domain to its object row.
    #[test]
    fn a_pick_selects_its_item_by_kind() {
        let project = crate::ui_bridge::force_project("RadialForce", &["force_a"]);
        let layer = LayerId::new("scene");
        let force = target(&project, &layer, GizmoTargetKind::Force);
        let ShapeOwner::Force(node) = &force.id.owner else { panic!("a force owner") };
        let scene = scene(&project, &layer);
        assert_eq!(scene_selection(&scene, &force), Some(SceneSelection::Force(node.clone())));
        let found = selected_targets(&scene, &[SceneSelection::Force(node.clone())], &[]).pop().expect("the force's shape");
        assert_eq!(found.id, force.id);

        let (fluid, fluid_layer) = fluid_project(false);
        let domain = target(&fluid, &fluid_layer, GizmoTargetKind::FluidDomain);
        let row = domain.row.expect("a domain belongs to its water object");
        let scene = self::scene(&fluid, &fluid_layer);
        assert_eq!(scene_selection(&scene, &domain), Some(SceneSelection::Object(row)));
        let column = gizmo_targets(&scene)
            .find(|t| t.kind == GizmoTargetKind::InitialFill)
            .expect("the GPU FLIP family places an Initial Fill");
        let ShapeOwner::RoleSource { node: fill, .. } = &column.id.owner else { panic!("an Initial Fill is a role source") };
        let fill = SceneSelection::Source(fill.clone());
        assert_eq!(scene_selection(&scene, &column), Some(fill.clone()), "a picked Initial Fill selects itself");
        assert_eq!(
            selected_targets(&scene, &[fill], std::slice::from_ref(&column.id)).pop().map(|t| t.id),
            Some(column.id.clone()),
            "a picked shape of the selected item keeps its handles"
        );
        assert_eq!(selected_targets(&scene, &[SceneSelection::Object(row)], &[]).pop().map(|t| t.id), gizmo_targets(&scene).find(|t| t.row == Some(row)).map(|t| t.id));
    }

    /// The Swarm template on a generator layer, its box moved off the scene
    /// origin so no other shape shares its pick point.
    fn swarm_project() -> (Project, LayerId) {
        let mut project = Project::default();
        let index = project.timeline.add_layer("Swarm", LayerType::Generator, PresetTypeId::from_string("Swarm".to_string()));
        let layer_id = project.timeline.layers[index].layer_id.clone();
        let mut def: EffectGraphDef = serde_json::from_str(manifold_nodes::testkit::assets::ASSETS_GENERATOR_PRESETS_SWARM_JSON).unwrap();
        let bounds = def.nodes.iter_mut().find(|node| node.node_id == NodeId::new("swarm_bounds")).expect("the swarm's box");
        bounds.params.insert("pos_x".into(), SerializedParamValue::Float { value: 3.0 });
        bounds.params.insert("pos_y".into(), SerializedParamValue::Float { value: 2.0 });
        let generator = project.timeline.layers[index].gen_params_mut().unwrap();
        generator.graph = Some(def);
        generator.refresh_manifest_from_graph();
        (project, layer_id)
    }

    /// A swarm in the scene shows its forces box as a gizmo target placed by
    /// its bounds transform; a click on it picks the box and selects the
    /// swarm's object row, whose handles stay on the box.
    #[test]
    fn a_swarm_box_is_a_target_and_picking_it_selects_the_swarm_row() {
        let (mut project, layer) = swarm_project();
        let swarm = target(&project, &layer, GizmoTargetKind::Swarm);
        assert_eq!(swarm.id.owner, ShapeOwner::Swarm(NodeId::new("swarm_forces")));
        let scene = scene(&project, &layer);
        let shape = scene.shapes.iter().find(|shape| shape.owner == swarm.id.owner).expect("the swarm's box shape");
        let place = |id: &str| shape.params.iter().find(|p| p.id == id).map(|p| (p.value, p.write.clone())).unwrap();
        let bounds = scene_vm_node_doc_id(&project, &layer, "swarm_bounds");
        assert_eq!(place("pos_x"), (3.0, ShapeWrite::Node(ParamAddr { scope_path: Vec::new(), node_doc_id: bounds, param_id: "pos_x".into() })));
        assert_eq!(place("pos_y").0, 2.0);
        assert_eq!(place("scale_x").0, 4.0);
        let row = swarm.row.expect("the swarm box belongs to the swarm's object row");
        let object = scene_vm_node_doc_id(&project, &layer, "swarm_object");
        assert_eq!(row, object, "the row is the scene object the swarm's copies feed");
        let modelled = scene.objects.iter().find_map(|object| match object {
            SceneObjectVm::Known(known) if known.object_node_id == row => known.swarm.as_ref().map(|swarm| swarm.node.clone()),
            _ => None,
        });
        assert_eq!(modelled, Some(NodeId::new("swarm_forces")), "the scene object model gives the row its swarm");

        let view = GizmoView::new(camera(), W, H);
        let mut rig = Rig::new(&layer);
        let picked = rig.with_ctx(&mut project, |ctx| tap(ctx, &layer, &scene, &[], false, &view, px(swarm.origin)));
        let GizmoPress::Picked(Some(mut ids)) = picked else { panic!("the swarm box is picked, got {picked:?}") };
        let id = ids.pop().unwrap();
        assert_eq!(id, swarm.id);
        let picked = gizmo_target_for(&scene, &id).expect("the picked target");
        assert_eq!(scene_selection(&scene, &picked), Some(SceneSelection::Object(row)));
        assert_eq!(selected_targets(&scene, &[SceneSelection::Object(row)], std::slice::from_ref(&id)).pop().map(|t| t.id), Some(id));
    }

    /// The authored doc id of the root node named `node_id`.
    fn scene_vm_node_doc_id(project: &Project, layer: &LayerId, node_id: &str) -> u32 {
        let def = authored_def(project, &GraphTarget::Generator(layer.clone())).unwrap();
        def.nodes.iter().find(|node| node.node_id == NodeId::new(node_id)).map(|node| node.id).unwrap()
    }
}
