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
use manifold_node_engine::scene::camera::Camera;
use manifold_nodes_scene::node_graph::scene_vm::{ParamAddr, ShapeWrite};
use manifold_nodes_scene::node_graph::viewport_gizmo::{GizmoDrag, HandleValues, HandleWrite};
use manifold_ui::{GraphParamTarget, ScrubPhase, ScrubValue, ValueRef};

use crate::app_render::UnboundNodeParamDrag;
use crate::content_command::ContentCommand;
use crate::ui_bridge::DispatchCtx;
use crate::ui_bridge::scrub::{ResolvedScrub, begin_param_triple, commit_param_triple, dispatch_scrub, move_param_triple};

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
    let Some(metadata) = def.preset_metadata.as_mut() else {
        return Some(());
    };
    let Some(owner) = project.graph_target_owner(target) else {
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
    knobs: Vec<Knob>,
    moved: bool,
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
        let gesture = Self { layer, drag, knobs, moved: false };
        match (&gesture.drag.handle().write, gesture.knobs.as_slice()) {
            (HandleWrite::Direction { .. }, [Knob::Card { id: x, .. }, Knob::Card { id: y, .. }, Knob::Card { id: z, .. }]) => {
                if !begin_param_triple(ctx, target, [x.clone(), y.clone(), z.clone()]) {
                    return Err("This direction cannot be turned here".into());
                }
            }
            (HandleWrite::Linear { .. } | HandleWrite::Angle(_), [Knob::Card { id, .. }]) => {
                dispatch_scrub(&gesture.card_ref(id), &ScrubPhase::Begin, ctx);
                if !matches!(&ctx.scrub.active, Some(ResolvedScrub::Param { param_id, .. }) if param_id == id) {
                    return Err("This handle's control is locked".into());
                }
            }
            (HandleWrite::Linear { param, .. } | HandleWrite::Angle(param), [Knob::Node(addr)]) => {
                let catalog = crate::ui_bridge::generator_catalog_default(&ctx.project, &gesture.layer)
                    .ok_or("Generator graph is unavailable")?;
                let pre_drag_value = ctx
                    .project
                    .graph_for_target(&target, Some(&catalog))
                    .and_then(|def| crate::editor_bridge::node_param_value(def, &addr.scope_path, addr.node_doc_id, &addr.param_id));
                let current = pre_drag_value.clone().unwrap_or(SerializedParamValue::Float { value: param.value });
                ctx.scrub.check_single_active_on_begin("gizmo-node-param");
                ctx.scrub.active = Some(ResolvedScrub::UnboundNodeParam(Box::new(UnboundNodeParamDrag::new(
                    target,
                    addr.scope_path.clone(),
                    addr.node_doc_id,
                    addr.param_id.clone(),
                    catalog,
                    pre_drag_value,
                    current,
                ))));
            }
            (HandleWrite::Linear { .. } | HandleWrite::Angle(_) | HandleWrite::Direction { .. }, _) => {
                return Err("A direction turns only through its card controls".into());
            }
        }
        Ok(gesture)
    }

    fn card_ref(&self, id: &ParamId) -> ValueRef {
        ValueRef::Param(GraphParamTarget::GeneratorOf(self.layer.clone()), id.clone())
    }

    /// Move the handle with the pointer at `cur` (render pixels): a live,
    /// non-undoable write through each knob.
    pub(crate) fn drag_to(&mut self, ctx: &mut DispatchCtx, cam: &Camera, width: u32, height: u32, cur: (f32, f32)) {
        let Some(values) = self.drag.update(cam, width, height, cur) else { return };
        match (values, self.knobs.as_slice()) {
            (HandleValues::One(value), [knob @ Knob::Card { id, .. }]) => {
                if let Some(card) = knob.card_value(value) {
                    dispatch_scrub(&self.card_ref(id), &ScrubPhase::Move(ScrubValue::Scalar(card)), ctx);
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
                    move_param_triple(ctx, [x, y, z]);
                }
            }
            (HandleValues::One(_) | HandleValues::Three(_), _) => return,
        }
        self.moved = true;
    }

    /// Release: one undo entry for the whole drag.
    pub(crate) fn commit(self, ctx: &mut DispatchCtx) {
        match self.knobs.as_slice() {
            [Knob::Card { id, .. }] => {
                dispatch_scrub(&self.card_ref(id), &ScrubPhase::Commit, ctx);
            }
            [Knob::Node(_)] => {
                if let Some(ResolvedScrub::UnboundNodeParam(drag)) = ctx.scrub.active.take()
                    && self.moved
                    && drag.moved()
                {
                    ContentCommand::send(ctx.content_tx, ContentCommand::Execute(Box::new(drag.into_command())));
                }
            }
            _ => commit_param_triple(ctx),
        }
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

        /// One full gesture: press `target`'s handle at `from`, drag to
        /// `to`, release.
        fn drag(&mut self, project: &mut Project, layer: &LayerId, target: &GizmoTarget, mode: GizmoMode, axis: GizmoAxis, from: (f32, f32), to: (f32, f32)) {
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
            let drag = GizmoDrag::begin(target.clone(), mode, axis, from).expect("handle is draggable");
            let mut gesture = GizmoGesture::begin(&mut ctx, layer.clone(), drag).expect("gesture begins");
            gesture.drag_to(&mut ctx, &camera(), W, H, to);
            gesture.commit(&mut ctx);
            assert!(ctx.scrub.active.is_none(), "release ends the scrub");
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
            .find(|node| node.node_id == NodeId::new("water_family"))
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
            def.preset_metadata
                .as_mut()
                .unwrap()
                .params
                .push(ParamSpecDef {
                    id: "domain_x".into(),
                    name: "Domain X".into(),
                    min: -10.0,
                    max: 10.0,
                    default_value: 0.0,
                    ..ParamSpecDef::default()
                });
            def.preset_metadata
                .as_mut()
                .unwrap()
                .bindings
                .push(BindingDef {
                    id: "domain_x".into(),
                    label: "Domain X".into(),
                    default_value: 0.0,
                    target: BindingTarget::Node {
                        node_id: NodeId::new("domain_transform"),
                        param: "pos_x".into(),
                    },
                    convert: ParamConvert::Float,
                    user_added: false,
                    scale: 1.0,
                    offset: 0.0,
                    default_mirrors_node_param: false,
                });
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
        rig.drag(&mut local, &layer, &domain, GizmoMode::Scale, GizmoAxis::Y, px(domain.origin), px(plus(domain.origin, [0.0, 1.0, 0.0])));
        let mut service = EditingService::new();
        assert_eq!(rig.apply(&mut service, &mut content), 1, "one undo entry for the drag");
        let value = |project: &Project| {
            let graph = GraphTarget::Generator(layer.clone());
            let def = crate::graph_target::resolve(project, &graph).unwrap();
            crate::editor_bridge::node_param_value(&def, &addr.scope_path, addr.node_doc_id, &addr.param_id)
        };
        for project in [&local, &content] {
            assert_eq!(value(project), Some(SerializedParamValue::Float { value: 5.0 }), "scale_y grew by one");
        }
        assert!(service.undo(&mut content));
        assert_eq!(value(&content), Some(SerializedParamValue::Float { value: 4.0 }));
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
}
