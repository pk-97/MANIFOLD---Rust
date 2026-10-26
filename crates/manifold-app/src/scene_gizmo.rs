//! Gizmo drags as knob scrubs (REALTIME_3D_DESIGN.md D8): a gizmo handle
//! writes each param it names exactly as that param's card knob does
//! (`ValueRef::Param`); a direction's three card params move as one
//! gesture. A scene's shapes name only card params, so a drag never writes
//! the compiled graph (SCENE_DOCUMENT_DESIGN.md ruling 18). Live writes
//! while dragging, one undo entry on release, no private path.
//!
//! The scene VM here is what the viewport reads: the scene document at the
//! layer's card values, so handles sit at the values the knobs show.

use std::sync::Arc;

use manifold_core::effects::{ParamId, invert_card_reshape};
use manifold_core::macro_bank::MacroCurve;
use manifold_core::scene_doc::ItemId;
use manifold_core::{GraphTarget, LayerId, project::Project};
use manifold_nodes_scene::node_graph::scene_vm::{SceneStructure, SceneVm, ShapeOwner, ShapeWrite};
use manifold_nodes_scene::node_graph::viewport_gizmo::{
    GizmoDrag, GizmoMode, GizmoSelection, GizmoTarget, HandleValues, HandleWrite, SelectionWrite,
    gizmo_target_for, gizmo_targets, pick_axis, pick_target, selection_writes,
};
use manifold_ui::panels::scene_setup_panel::SceneSelection;
use manifold_ui::{GraphParamTarget, ScrubPhase, ScrubValue, ValueRef};

use crate::content_command::ContentCommand;
use crate::ui_bridge::DispatchCtx;
use crate::ui_bridge::scrub::{
    ResolvedScrub, begin_param_group, commit_param_group, dispatch_scrub, move_param_group,
};

/// The scene VM of `layer`'s scene document at the layer's card values, so
/// handles sit where the knobs show. `None` when the layer has no scene.
pub(crate) fn scene_vm(project: &Project, layer: &LayerId) -> Option<SceneVm> {
    layer_scene_vm(project.timeline.find_layer_by_id(layer.as_str())?.1)
}

/// The one way the app builds a scene VM (SCENE_DOCUMENT_DESIGN.md section
/// 7.1 (The scene document)): the layer's document at its card values.
/// Gizmos, Frame and the setup check all read it.
pub(crate) fn layer_scene_vm(layer: &manifold_core::layer::Layer) -> Option<SceneVm> {
    layer_scene_vm_over(layer, None)
}

/// [`layer_scene_vm`] over `structure` when the caller holds the structure
/// of this same document (only values moved since), so a value write never
/// lays out the compile again.
pub(crate) fn layer_scene_vm_over(
    layer: &manifold_core::layer::Layer,
    structure: Option<&Arc<SceneStructure>>,
) -> Option<SceneVm> {
    let inst = layer.gen_params()?;
    let doc = inst.scene.as_ref()?;
    let structure = structure.map_or_else(|| SceneStructure::of(doc), Arc::clone);
    Some(SceneVm::with_structure(structure, doc, &|id| {
        inst.params.contains(id).then(|| inst.get_base_param(id))
    }))
}

/// [`scene_vm`] for a graph target: only a generator layer holds a scene.
/// `structure`, when given, is this target's document structure.
pub(crate) fn scene_vm_of(
    project: &Project,
    target: &GraphTarget,
    structure: Option<&Arc<SceneStructure>>,
) -> Option<SceneVm> {
    match target {
        GraphTarget::Generator(layer) => layer_scene_vm_over(
            project.timeline.find_layer_by_id(layer.as_str())?.1,
            structure,
        ),
        _ => None,
    }
}

/// The card knob a shape param has, with the reshape from its value to the
/// shape's.
#[derive(Debug, Clone, PartialEq)]
struct Knob {
    id: ParamId,
    min: f32,
    max: f32,
    invert: bool,
    curve: MacroCurve,
    scale: f32,
    offset: f32,
}

impl Knob {
    /// The knob value that places the shape param at `value`.
    fn card_value(&self, value: f32) -> Option<f32> {
        invert_card_reshape(
            value,
            self.min,
            self.max,
            self.invert,
            self.curve,
            self.scale,
            self.offset,
        )
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
    cards: Vec<f32>,
    writes: Vec<SelectionWrite>,
}

#[derive(Debug, Clone, PartialEq)]
struct MemberParam {
    member: usize,
    id: String,
    knob: Knob,
    /// Where the param sits in the group scrub's card order.
    slot: usize,
}

impl GizmoGesture {
    /// Resolve each param's knob and begin its scrub. `Err` carries why the
    /// handle cannot be dragged.
    pub(crate) fn begin(
        ctx: &mut DispatchCtx,
        layer: LayerId,
        drag: GizmoDrag,
    ) -> Result<Self, String> {
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
            (HandleWrite::Direction { .. }, [x, y, z]) => {
                if !begin_param_group(
                    ctx,
                    target,
                    vec![x.id.clone(), y.id.clone(), z.id.clone()],
                    Vec::new(),
                    "Turn direction",
                ) {
                    return Err("This direction cannot be turned here".into());
                }
            }
            (HandleWrite::Uniform { .. }, [x, y, z]) => {
                if !begin_param_group(
                    ctx,
                    target,
                    vec![x.id.clone(), y.id.clone(), z.id.clone()],
                    Vec::new(),
                    "Scale copies",
                ) {
                    return Err("This scale cannot be changed here".into());
                }
            }
            (HandleWrite::Linear { .. } | HandleWrite::Angle(_), [Knob { id, .. }]) => {
                dispatch_scrub(&card_ref(&layer, id), &ScrubPhase::Begin, ctx);
                if !matches!(&ctx.scrub.active, Some(ResolvedScrub::Param { param_id, .. }) if param_id == id)
                {
                    return Err("This handle's control is locked".into());
                }
            }
            (
                HandleWrite::Linear { .. }
                | HandleWrite::Angle(_)
                | HandleWrite::Direction { .. }
                | HandleWrite::Uniform { .. },
                _,
            ) => {
                return Err(
                    "A direction or a uniform scale moves only through its card controls".into(),
                );
            }
            (HandleWrite::Selection { .. }, _) => {
                return Err("The selection's handle moves through its members".into());
            }
        }
        Ok(Self {
            layer,
            drag,
            kind: GestureKind::Shape(knobs),
            moved: false,
        })
    }

    /// Begin dragging the selection's handle over `members`: every param a
    /// member can write, through its knob, as one gesture. A wired or
    /// macro-locked param, and an unplaced member, stay put. `Err` carries
    /// why nothing can move.
    pub(crate) fn begin_selection(
        ctx: &mut DispatchCtx,
        layer: LayerId,
        drag: GizmoDrag,
        members: Vec<GizmoTarget>,
    ) -> Result<Self, String> {
        let target = GraphTarget::Generator(layer.clone());
        let (mut params, mut cards, mut card_base) =
            (Vec::new(), Vec::<ParamId>::new(), Vec::new());
        for (member, shape) in members
            .iter()
            .enumerate()
            .filter(|(_, shape)| !shape.unplaced)
        {
            for param in shape
                .handles
                .iter()
                .flat_map(|handle| handle.write.params())
            {
                if param.wired
                    || params
                        .iter()
                        .any(|seen: &MemberParam| seen.member == member && seen.id == param.id)
                {
                    continue;
                }
                let Some(knob) = knob(&ctx.project, &target, &param.write) else {
                    continue;
                };
                if crate::graph_target::macro_parameter_lock_reason(
                    &ctx.project,
                    &target,
                    knob.id.as_ref(),
                )
                .is_some()
                {
                    continue;
                }
                let slot = match cards.iter().position(|card| *card == knob.id) {
                    Some(at) => at,
                    None => {
                        let Some(base) = ctx.project.with_preset_graph(&target, |inst| {
                            inst.get_base_param(knob.id.as_ref())
                        }) else {
                            continue;
                        };
                        cards.push(knob.id.clone());
                        card_base.push(base);
                        cards.len() - 1
                    }
                };
                params.push(MemberParam {
                    member,
                    id: param.id.clone(),
                    knob,
                    slot,
                });
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
        if !begin_param_group(ctx, target, cards, Vec::new(), label) {
            return Err("The selection cannot move here".into());
        }
        let selection = SelectionGesture {
            members,
            params,
            cards: card_base.clone(),
            card_base,
            writes: Vec::new(),
        };
        Ok(Self {
            layer,
            drag,
            kind: GestureKind::Selection(Box::new(selection)),
            moved: false,
        })
    }

    /// Move the handle with the pointer at `cur` (render pixels of `view`):
    /// a live, non-undoable write through each knob.
    pub(crate) fn drag_to(&mut self, ctx: &mut DispatchCtx, view: &GizmoView, cur: (f32, f32)) {
        let Some(values) = self.drag.update(view, cur) else {
            return;
        };
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
            (HandleValues::One(value), [knob]) => {
                if let Some(card) = knob.card_value(value) {
                    dispatch_scrub(
                        &card_ref(&self.layer, &knob.id),
                        &ScrubPhase::Move(ScrubValue::Scalar(card)),
                        ctx,
                    );
                }
            }
            (HandleValues::Three(values), [x, y, z]) => {
                if let (Some(x), Some(y), Some(z)) = (
                    x.card_value(values[0]),
                    y.card_value(values[1]),
                    z.card_value(values[2]),
                ) {
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
            [Knob { id, .. }] => {
                dispatch_scrub(&card_ref(&self.layer, id), &ScrubPhase::Commit, ctx);
            }
            _ => commit_param_group(ctx),
        }
    }
}

impl SelectionGesture {
    /// Every member's params with the handle dragged by `amount`, each from
    /// its value at the press, written live through the group scrub.
    fn move_to(
        &mut self,
        ctx: &mut DispatchCtx,
        mode: GizmoMode,
        axis: manifold_nodes_scene::node_graph::viewport_gizmo::GizmoAxis,
        amount: f32,
    ) {
        selection_writes(&self.members, mode, axis, amount, &mut self.writes);
        self.cards.copy_from_slice(&self.card_base);
        for write in &self.writes {
            let Some(param) = self
                .params
                .iter()
                .find(|param| param.member == write.member && param.id == write.param.id)
            else {
                continue;
            };
            if let Some(card) = param.knob.card_value(write.value) {
                self.cards[param.slot] = card;
            }
        }
        move_param_group(ctx, &self.cards, &[]);
    }
}

fn card_ref(layer: &LayerId, id: &ParamId) -> ValueRef {
    ValueRef::Param(GraphParamTarget::GeneratorOf(layer.clone()), id.clone())
}

pub(crate) use manifold_nodes_scene::node_graph::viewport_gizmo::GizmoView;

/// What a left press on a gizmo host did.
#[derive(Debug)]
pub(crate) enum GizmoPress {
    /// The selection's handle took the press: `Some` when its drag began.
    /// A locked handle, or one whose drag cannot begin, still takes the
    /// press, so it never falls through to what lies under it.
    Handle(Option<Box<GizmoGesture>>),
    /// No handle there. `true` when the press picked something that draws
    /// no pixels (a lamp, a force, a water box or source) and selected it in
    /// the Scene panel; `false` otherwise, and the host picks what is drawn
    /// there through the item-id buffer (`scene_pick.rs`).
    Picked(bool),
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
/// selection's one handle set first, then a pick of a helper that draws no
/// pixels. With `at.extend` (Shift) the pick adds its item to the Scene
/// panel's selection or takes it out. `scene` is the host's cached
/// `SceneVm` of `layer`'s scene document and `selection` was built over it.
pub(crate) fn press(
    ctx: &mut DispatchCtx,
    layer: &LayerId,
    scene: &SceneVm,
    selection: &GizmoSelection,
    mode: GizmoMode,
    at: PressAt<'_>,
) -> GizmoPress {
    let PressAt {
        view,
        point,
        extend,
    } = at;
    if let Some(target) = selection.handles.clone()
        && let Some(axis) = pick_axis(mode, &target, view, point)
    {
        // D8: a locked handle draws gray and refuses the drag.
        if target
            .handle(mode, axis)
            .is_none_or(|handle| handle.write.locked())
        {
            return GizmoPress::Handle(None);
        }
        if selection.selected.len() > 1 {
            let Some(drag) = GizmoDrag::begin(target, mode, axis, view, point) else {
                return GizmoPress::Handle(None);
            };
            return GizmoPress::Handle(
                match GizmoGesture::begin_selection(
                    ctx,
                    layer.clone(),
                    drag,
                    selection.selected.clone(),
                ) {
                    Ok(gesture) => Some(Box::new(gesture)),
                    Err(message) => {
                        ContentCommand::send(
                            ctx.content_tx,
                            ContentCommand::GraphEditRejected(message),
                        );
                        None
                    }
                },
            );
        }
        // A shape with no transform yet cannot be dragged here; the lane that
        // declares such shapes places them.
        if target.unplaced {
            return GizmoPress::Handle(None);
        }
        let Some(drag) = GizmoDrag::begin(target, mode, axis, view, point) else {
            return GizmoPress::Handle(None);
        };
        return GizmoPress::Handle(match GizmoGesture::begin(ctx, layer.clone(), drag) {
            Ok(gesture) => Some(Box::new(gesture)),
            Err(message) => {
                ContentCommand::send(ctx.content_tx, ContentCommand::GraphEditRejected(message));
                None
            }
        });
    }
    let Some(item) = pick_target(scene, view, point)
        .and_then(|id| gizmo_target_for(scene, &id))
        .and_then(|target| scene_selection(&target))
    else {
        return GizmoPress::Picked(false);
    };
    let panel = &mut ctx.ui.scene_setup_panel;
    if extend {
        panel.toggle_selected(layer, item);
    } else {
        panel.select_items(layer.clone(), vec![item]);
    }
    GizmoPress::Picked(true)
}

/// The shapes the Scene panel's selected `items` show: each item's first
/// declared shape.
pub(crate) fn selected_targets(scene: &SceneVm, items: &[SceneSelection]) -> Vec<GizmoTarget> {
    items
        .iter()
        .filter_map(|item| {
            gizmo_targets(scene).find(|target| scene_selection(target).as_ref() == Some(item))
        })
        .collect()
}

/// The Scene panel item a target belongs to: the row its shape selects.
pub(crate) fn scene_selection(target: &GizmoTarget) -> Option<SceneSelection> {
    target.row.clone().map(SceneSelection::Item)
}

/// The scene object a target outlines, without allocating: what the preview
/// outline reads every frame. Only an item's own shape draws pixels.
pub(crate) fn selected_object(target: &GizmoTarget) -> Option<&ItemId> {
    match target.id.owner {
        ShapeOwner::Item(_) => target.row.as_ref(),
        _ => None,
    }
}

/// The card knob behind `write`; `None` for a shape not placed yet.
fn knob(project: &Project, target: &GraphTarget, write: &ShapeWrite) -> Option<Knob> {
    let owner = project.graph_target_owner(target)?;
    match write {
        ShapeWrite::Card { id, scale, offset } => {
            let spec = &owner.params.get(id)?.spec;
            Some(Knob {
                id: ParamId::from(id.clone()),
                min: spec.min,
                max: spec.max,
                invert: spec.invert,
                curve: spec.curve,
                scale: *scale,
                offset: *offset,
            })
        }
        ShapeWrite::Unplaced => None,
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
        fn with_ctx<R>(
            &mut self,
            project: &mut Project,
            f: impl FnOnce(&mut DispatchCtx) -> R,
        ) -> R {
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
                    ContentCommand::Execute(cmd) => {
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
        Camera::look_at(
            [0.0, 2.0, 14.0],
            [0.0, 2.0, 0.0],
            [0.0, 1.0, 0.0],
            0.9,
            0.1,
            100.0,
        )
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
        let index = project
            .timeline
            .add_layer("Scene", LayerType::Generator, PresetTypeId::SCENE);
        let layer = project.timeline.layers[index].layer_id.clone();
        let mut edit = crate::scene_edit::add(&project, &layer, "object.cube", None, &[])
            .expect("a cube adds to the base scene");
        edit.command.execute(&mut project);
        let second = edit.select.expect("the add selects the new cube");
        project.with_preset_graph_mut(&GraphTarget::Generator(layer.clone()), |inst| {
            inst.set_base_param(&param_id(&second, "pos_x"), 6.0);
        });
        (project, layer, ItemId::new("cube"), second)
    }

    fn target_of(scene: &SceneVm, item: &ItemId) -> GizmoTarget {
        gizmo_targets(scene)
            .find(|target| target.row.as_ref() == Some(item))
            .expect("the item declares a shape")
    }

    fn base(project: &Project, layer: &LayerId, item: &ItemId, property: &str) -> f32 {
        project
            .graph_target_owner(&GraphTarget::Generator(layer.clone()))
            .unwrap()
            .get_base_param(&param_id(item, property))
    }

    /// One handle drag writes the item's card and lands as one undo entry.
    #[test]
    fn a_handle_drag_scrubs_the_card_and_undoes_in_one_step() {
        let (mut local, layer, first, _) = two_cubes();
        let mut content = local.clone();
        let scene = scene_vm(&local, &layer).unwrap();
        let target = target_of(&scene, &first);
        let view = GizmoView::new(camera(), W, H);
        let grip = target
            .grip(GizmoMode::Move, GizmoAxis::X, &view)
            .expect("the cube's X handle");
        let mut rig = Rig::new(&layer);
        rig.with_ctx(&mut local, |ctx| {
            let drag = GizmoDrag::begin(
                target.clone(),
                GizmoMode::Move,
                GizmoAxis::X,
                &view,
                px(grip),
            )
            .expect("handle is draggable");
            let mut gesture =
                GizmoGesture::begin(ctx, layer.clone(), drag).expect("gesture begins");
            gesture.drag_to(ctx, &view, px(plus(grip, [1.0, 0.0, 0.0])));
            gesture.commit(ctx);
            assert!(ctx.scrub.active.is_none(), "release ends the scrub");
        });
        let mut service = EditingService::new();
        assert_eq!(
            rig.apply(&mut service, &mut content),
            1,
            "one undo entry for the drag"
        );
        assert!(
            base(&local, &layer, &first, "pos_x").abs() < 1e-3,
            "the UI copy keeps the pre-drag value"
        );
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
        let selection = GizmoSelection::new(&scene, vec![a, b]);
        let group = selection
            .handles
            .clone()
            .expect("one handle set for the selection");
        let grip = group
            .grip(GizmoMode::Move, GizmoAxis::X, &view)
            .expect("the selection's X handle");
        let mut rig = Rig::new(&layer);
        rig.with_ctx(&mut local, |ctx| {
            let at = PressAt {
                view: &view,
                point: px(grip),
                extend: false,
            };
            let GizmoPress::Handle(Some(mut gesture)) =
                press(ctx, &layer, &scene, &selection, GizmoMode::Move, at)
            else {
                panic!("the selection's handle takes the press")
            };
            gesture.drag_to(ctx, &view, px(plus(grip, [0.5, 0.0, 0.0])));
            gesture.drag_to(ctx, &view, px(plus(grip, [1.0, 0.0, 0.0])));
            gesture.commit(ctx);
            assert!(ctx.scrub.active.is_none(), "release ends the scrub");
        });
        let mut service = EditingService::new();
        assert_eq!(
            rig.apply(&mut service, &mut content),
            1,
            "one undo entry for the whole selection"
        );
        assert!(
            base(&local, &layer, &first, "pos_x").abs() < 1e-3,
            "the UI copy keeps the pre-drag value"
        );
        assert!(
            (base(&content, &layer, &first, "pos_x") - 1.0).abs() < 1e-3,
            "the first cube moved one unit"
        );
        assert!(
            (base(&content, &layer, &second, "pos_x") - 7.0).abs() < 1e-3,
            "the second cube moved one unit"
        );
        assert!(service.undo(&mut content));
        assert_eq!(base(&content, &layer, &first, "pos_x"), 0.0);
        assert_eq!(base(&content, &layer, &second, "pos_x"), 6.0);
    }
}
