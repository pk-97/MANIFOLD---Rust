//! Gizmo drags as knob scrubs (REALTIME_3D_DESIGN.md D8): a gizmo handle
//! writes each param it names exactly as that param's knob does. A param
//! bound to a card scrubs the card (`ValueRef::Param`); an unbound node
//! param scrubs as the graph node face does; a direction's three card
//! params move as one gesture. Live writes while dragging, one undo entry
//! on release, no private path.
//!
//! The scene VM here is what the viewport reads: the scene document at the
//! layer's card values, so handles sit at the values the knobs show.

use manifold_core::effects::{ParamId, invert_card_reshape};
use manifold_core::macro_bank::MacroCurve;
use manifold_core::scene_doc::ItemId;
use manifold_editing::command::Command;
use manifold_core::{GraphTarget, LayerId, project::Project};
use manifold_core::effect_graph_def::SerializedParamValue;
use manifold_nodes_scene::node_graph::scene_vm::{ParamAddr, SceneVm, ShapeOwner, ShapeWrite};
use manifold_nodes_scene::node_graph::viewport_gizmo::{
    GizmoDrag, GizmoMode, GizmoSelection, GizmoTarget, GizmoTargetId, HandleValues, HandleWrite, SelectionWrite,
    gizmo_target_for, gizmo_targets, pick_axis, pick_target, selection_writes, toggle_pick,
};
use manifold_ui::panels::scene_setup_panel::SceneSelection;
use manifold_ui::{GraphParamTarget, ScrubPhase, ScrubValue, ValueRef};

use crate::app_render::UnboundNodeParamDrag;
use crate::content_command::ContentCommand;
use crate::ui_bridge::DispatchCtx;
use crate::ui_bridge::scrub::{ResolvedScrub, begin_param_group, commit_param_group, dispatch_scrub, move_param_group};

/// The scene VM of `layer`'s scene document at the layer's card values, so
/// handles sit where the knobs show. `None` when the layer has no scene.
pub(crate) fn scene_vm(project: &Project, layer: &LayerId) -> Option<SceneVm> {
    let doc = project.scene_of(layer)?;
    let inst = project.timeline.find_layer_by_id(layer.as_str())?.1.gen_params()?;
    Some(SceneVm::from_doc(doc, &|id| inst.params.contains(id).then(|| inst.get_base_param(id))))
}

/// [`scene_vm`] for a graph target: only a generator layer holds a scene.
pub(crate) fn scene_vm_of(project: &Project, target: &GraphTarget) -> Option<SceneVm> {
    match target {
        GraphTarget::Generator(layer) => scene_vm(project, layer),
        _ => None,
    }
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
            (HandleWrite::Uniform { .. }, [Knob::Card { id: x, .. }, Knob::Card { id: y, .. }, Knob::Card { id: z, .. }]) => {
                if !begin_param_group(ctx, target, vec![x.clone(), y.clone(), z.clone()], Vec::new(), "Scale copies") {
                    return Err("This scale cannot be changed here".into());
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
            (HandleWrite::Linear { .. } | HandleWrite::Angle(_) | HandleWrite::Direction { .. } | HandleWrite::Uniform { .. }, _) => {
                return Err("A direction or a uniform scale moves only through its card controls".into());
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
                        if crate::graph_target::macro_parameter_lock_reason(&ctx.project, &target, id.as_ref()).is_some() {
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
        // A shape with no transform yet cannot be dragged here; the lane that
        // declares such shapes places them.
        if target.unplaced {
            return GizmoPress::Handle(None);
        }
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
    for item in ids.iter().filter_map(|id| gizmo_target_for(scene, id)).filter_map(|target| scene_selection(&target)) {
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
        let belongs = |target: &GizmoTarget| scene_selection(target).as_ref() == Some(item);
        let before = out.len();
        out.extend(picked.iter().filter_map(|id| gizmo_target_for(scene, id)).filter(belongs));
        if out.len() == before {
            out.extend(gizmo_targets(scene).find(belongs));
        }
    }
    out
}

/// The Scene panel item a target belongs to: an object or group shape selects
/// its own row. Forces, role sources and the other owners the water, swarm and
/// force lanes bring select through their own rows.
pub(crate) fn scene_selection(target: &GizmoTarget) -> Option<SceneSelection> {
    selected_object(target).cloned().map(SceneSelection::Item)
}

/// The item row a target selects, without allocating: what the preview
/// outline reads every frame.
pub(crate) fn selected_object(target: &GizmoTarget) -> Option<&ItemId> {
    match target.id.owner {
        ShapeOwner::Item(_) => target.row.as_ref(),
        _ => None,
    }
}

/// The knob behind `write`: a host card, or the node param itself.
fn knob(project: &Project, target: &GraphTarget, write: &ShapeWrite) -> Option<Knob> {
    let owner = project.graph_target_owner(target)?;
    match write {
        ShapeWrite::Card { id, scale, offset } => {
            let spec = &owner.params.get(id)?.spec;
            Some(Knob::Card { id: ParamId::from(id.clone()), min: spec.min, max: spec.max, invert: spec.invert, curve: spec.curve, scale: *scale, offset: *offset })
        }
        ShapeWrite::Unplaced => None,
        ShapeWrite::Node(addr) => Some(Knob::Node(addr.clone())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::SelectionState;
    use crate::ui_root::UIRoot;
    use manifold_core::PresetTypeId;
    use manifold_core::scene_doc::param_id;
    use manifold_core::types::LayerType;
    use manifold_editing::service::EditingService;
    use manifold_node_engine::scene::camera::Camera;
    use manifold_nodes_scene::node_graph::viewport_gizmo::{GizmoAxis, GizmoMode, gizmo_targets};

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

    /// The base scene's cube and a second cube six units along X, clear of the first one's handles.
    fn two_cubes() -> (Project, LayerId, ItemId, ItemId) {
        let mut project = Project::default();
        let index = project.timeline.add_layer("Scene", LayerType::Generator, PresetTypeId::SCENE);
        let layer = project.timeline.layers[index].layer_id.clone();
        let mut edit = crate::scene_edit::add(&project, &layer, "object.cube", None).expect("a cube adds to the base scene");
        edit.command.execute(&mut project);
        let second = edit.select.expect("the add selects the new cube");
        project.with_preset_graph_mut(&GraphTarget::Generator(layer.clone()), |inst| {
            inst.set_base_param(&param_id(&second, "pos_x"), 6.0);
        });
        (project, layer, ItemId::new("cube"), second)
    }

    fn target_of(scene: &SceneVm, item: &ItemId) -> GizmoTarget {
        gizmo_targets(scene).find(|target| target.row.as_ref() == Some(item)).expect("the item declares a shape")
    }

    fn base(project: &Project, layer: &LayerId, item: &ItemId, property: &str) -> f32 {
        project.graph_target_owner(&GraphTarget::Generator(layer.clone())).unwrap().get_base_param(&param_id(item, property))
    }

    /// A press at `point` with the shapes `selected` selected, as a host
    /// presses; `extend` is Shift.
    fn tap(ctx: &mut DispatchCtx, layer: &LayerId, scene: &SceneVm, selected: &[GizmoTargetId], extend: bool, view: &GizmoView, point: (f32, f32)) -> GizmoPress {
        let selection = GizmoSelection::of_ids(scene, selected);
        press(ctx, layer, scene, &selection, GizmoMode::Move, PressAt { view, point, extend })
    }

    /// A pick selects its item's row in the Scene panel.
    #[test]
    fn a_pick_selects_its_item() {
        let (mut project, layer, first, _) = two_cubes();
        let scene = scene_vm(&project, &layer).unwrap();
        let view = GizmoView::new(camera(), W, H);
        let target = target_of(&scene, &first);
        let mut rig = Rig::new(&layer);
        let picked = rig.with_ctx(&mut project, |ctx| tap(ctx, &layer, &scene, &[], false, &view, px(target.origin)));
        assert!(matches!(&picked, GizmoPress::Picked(Some(ids)) if *ids == [target.id.clone()]), "the cube is picked: {picked:?}");
        assert_eq!(rig.ui.scene_setup_panel.live_selected(), [], "no panel is open to follow");
        assert_eq!(scene_selection(&target), Some(SceneSelection::Item(first)));
    }

    /// Shift-click adds a shape to the selection, a second Shift-click on
    /// it takes it out, and a plain click selects one.
    #[test]
    fn shift_click_adds_and_removes_a_shape() {
        let (mut project, layer, first, second) = two_cubes();
        let scene = scene_vm(&project, &layer).unwrap();
        let (a, b) = (target_of(&scene, &first), target_of(&scene, &second));
        let view = GizmoView::new(camera(), W, H);
        let mut rig = Rig::new(&layer);
        let mut tap_at = |selected: &[GizmoTargetId], extend: bool, point: (f32, f32)| match rig.with_ctx(&mut project, |ctx| tap(ctx, &layer, &scene, selected, extend, &view, point)) {
            GizmoPress::Picked(Some(ids)) => ids,
            other => panic!("a pick, got {other:?}"),
        };
        let both = tap_at(std::slice::from_ref(&a.id), true, px(b.origin));
        assert_eq!(both, [a.id.clone(), b.id.clone()], "Shift adds");
        assert_eq!(tap_at(&both, true, px(b.origin)), std::slice::from_ref(&a.id), "Shift on a selected shape takes it out");
        assert_eq!(tap_at(&both, false, px(b.origin)), std::slice::from_ref(&b.id), "a plain click selects one");
    }

    /// One handle drag writes the item's card and lands as one undo entry.
    #[test]
    fn a_handle_drag_scrubs_the_card_and_undoes_in_one_step() {
        let (mut local, layer, first, _) = two_cubes();
        let mut content = local.clone();
        let scene = scene_vm(&local, &layer).unwrap();
        let target = target_of(&scene, &first);
        let view = GizmoView::new(camera(), W, H);
        let grip = target.grip(GizmoMode::Move, GizmoAxis::X, &view).expect("the cube's X handle");
        let mut rig = Rig::new(&layer);
        rig.with_ctx(&mut local, |ctx| {
            let drag = GizmoDrag::begin(target.clone(), GizmoMode::Move, GizmoAxis::X, &view, px(grip)).expect("handle is draggable");
            let mut gesture = GizmoGesture::begin(ctx, layer.clone(), drag).expect("gesture begins");
            gesture.drag_to(ctx, &view, px(plus(grip, [1.0, 0.0, 0.0])));
            gesture.commit(ctx);
            assert!(ctx.scrub.active.is_none(), "release ends the scrub");
        });
        let mut service = EditingService::new();
        assert_eq!(rig.apply(&mut service, &mut content), 1, "one undo entry for the drag");
        assert!((base(&local, &layer, &first, "pos_x") - 1.0).abs() < 1e-3);
        assert!((base(&content, &layer, &first, "pos_x") - 1.0).abs() < 1e-3);
        assert!(service.undo(&mut content));
        assert_eq!(base(&content, &layer, &first, "pos_x"), 0.0);
    }

    /// One handle set moves the whole selection: both cubes' cards, one undo
    /// entry, and undo puts both back.
    #[test]
    fn the_selection_moves_as_one_undo_step() {
        let (mut local, layer, first, second) = two_cubes();
        let mut content = local.clone();
        let scene = scene_vm(&local, &layer).unwrap();
        let (a, b) = (target_of(&scene, &first), target_of(&scene, &second));
        let view = GizmoView::new(camera(), W, H);
        let selection = GizmoSelection::of_ids(&scene, &[a.id.clone(), b.id.clone()]);
        let group = selection.handles.clone().expect("one handle set for the selection");
        let grip = group.grip(GizmoMode::Move, GizmoAxis::X, &view).expect("the selection's X handle");
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
            assert!((base(project, &layer, &first, "pos_x") - 1.0).abs() < 1e-3, "the first cube moved one unit");
            assert!((base(project, &layer, &second, "pos_x") - 7.0).abs() < 1e-3, "the second cube moved one unit");
        }
        assert!(service.undo(&mut content));
        assert_eq!(base(&content, &layer, &first, "pos_x"), 0.0);
        assert_eq!(base(&content, &layer, &second, "pos_x"), 6.0);
    }
}
