use std::borrow::Cow;

use manifold_core::NodeId;
use manifold_core::fluid_domain::DOMAIN_EXTENT_METRES;
use manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID;
use manifold_foundation::scene_surface::{SceneShape, SceneShapeDecl};

use super::*;
use crate::node_graph::scene_surface::scene_shape;
use crate::node_graph::scene_vm::{ParamAddr, SceneHeaderVm, SceneObjectVm, ShapeWrite, TransformVm, object_shapes};

fn cam_looking_down_neg_z(pos: [f32; 3]) -> Camera {
    Camera::look_at(pos, [pos[0], pos[1], pos[2] - 1.0], [0.0, 1.0, 0.0], 0.9, 0.1, 100.0)
}

fn known_object(id: u32, pos: (f32, f32, f32), driven: (bool, bool, bool)) -> SceneObjectVm {
    let addr = |n: &str| ParamAddr { scope_path: Vec::new(), node_doc_id: 99, param_id: n.to_string() };
    SceneObjectVm::Known(Box::new(crate::node_graph::scene_vm::SceneObjectKnownRow {
        is_group: false,
        parent_group_id: None,
        index: 0,
        object_node_id: id,
        object: NodeId::new(format!("object{id}")),
        kind: Default::default(),
        controls: Vec::new(),
        capabilities: manifold_core::scene_object_model::SceneObjectCapabilities { transform: true, ..Default::default() },
        group_node_id: None,
        name: "Obj".to_string(),
        visible_addr: addr("visible"),
        visible_value: true,
        visible_driven: false,
        transform: Some(TransformVm {
            node_doc_id: 99,
            node: NodeId::new("transform"),
            pos_addr: (addr("pos_x"), addr("pos_y"), addr("pos_z")),
            pos_value: pos,
            pos_driven: driven,
            rot_addr: (addr("rot_x"), addr("rot_y"), addr("rot_z")),
            rot_value: (0.0, 0.0, 0.0),
            rot_driven: (false, false, false),
            scale_addr: (addr("scale_x"), addr("scale_y"), addr("scale_z")),
            scale_value: (1.0, 1.0, 1.0),
            scale_driven: (false, false, false),
        }),
        material: crate::node_graph::scene_vm::MaterialVm::None,
        transform_chain: Vec::new(),
        transform_chain_parseable: true,
        modifier_chain: Vec::new(),
        modifier_chain_parseable: true,
        skin: None,
        physics: None,
        physics_imported: false,
        liquid_domain: None,
        fluid_domain: None,
        fluid_domain_error: None,
        fluid_domain_transform: None,
        placed_sources: Vec::new(),
    }))
}

fn scene_with(objects: Vec<SceneObjectVm>) -> SceneVm {
    let shapes = object_shapes(&objects);
    scene_of(objects, shapes)
}

fn scene_of(objects: Vec<SceneObjectVm>, shapes: Vec<SceneShapeVm>) -> SceneVm {
    SceneVm {
        scene_root_node_id: 0,
        multiple_scenes: false,
        header: SceneHeaderVm::default(),
        objects,
        lights: Vec::new(),
        camera: crate::node_graph::scene_vm::CameraVm::None,
        camera_nodes: Vec::new(),
        environment: crate::node_graph::scene_vm::EnvironmentVm::None,
        atmosphere: crate::node_graph::scene_vm::AtmosphereVm::None,
        world_nodes: Vec::new(),
        physics_nodes: Vec::new(),
        scene_bounds: None,
        shapes,
        problems: Vec::new(),
        setup_anchors: Vec::new(),
    }
}

fn object_id(id: u32) -> GizmoTargetId {
    GizmoTargetId { owner: ShapeOwner::Object(id), shape: 0 }
}

/// A node shape whose params live on node 77, valued from `values` by id.
fn node_shape(owner: ShapeOwner, index: usize, decl: &SceneShapeDecl, values: &[(&str, f32)], wired: &[&str]) -> SceneShapeVm {
    let params = decl
        .shape
        .param_ids()
        .map(|id| ShapeParamVm {
            id: id.to_string(),
            write: ShapeWrite::Node(ParamAddr { scope_path: vec![9], node_doc_id: 77, param_id: id.to_string() }),
            value: values.iter().find(|(name, _)| *name == id).map_or(0.0, |(_, v)| *v),
            wired: wired.contains(&id),
        })
        .collect();
    SceneShapeVm { owner, index, decl: decl.clone(), params, frame: None, anchor: [0.0; 3], row: Some(4), pickable: true }
}

/// A force shape over host card params named `host_<id>`.
fn force_shape(decl: SceneShapeDecl, values: &[(&str, f32)], anchor: [f32; 3]) -> SceneShapeVm {
    let params = decl
        .shape
        .param_ids()
        .map(|id| ShapeParamVm {
            id: id.to_string(),
            write: ShapeWrite::Card { id: format!("host_{id}"), scale: 1.0, offset: 0.0 },
            value: values.iter().find(|(name, _)| *name == id).map_or(0.0, |(_, v)| *v),
            wired: false,
        })
        .collect();
    SceneShapeVm { owner: ShapeOwner::Force(NodeId::new("force")), index: 0, decl, params, frame: None, anchor, row: None, pickable: true }
}

fn manifest_decl(json: &str) -> SceneShapeDecl {
    serde_json::from_str(json).unwrap()
}

/// Drag `axis` of `mode` from the origin's pixel by `delta` pixels.
fn drag(target: &GizmoTarget, mode: GizmoMode, axis: GizmoAxis, from: (f32, f32), to: (f32, f32)) -> HandleValues {
    let cam = cam_looking_down_neg_z([0.0, 0.0, 0.0]);
    let view = GizmoView::new(cam, 640, 480);
    let mut drag = GizmoDrag::begin(target.clone(), mode, axis, &view, from).expect("handle is draggable");
    drag.update(&view, to).expect("drag resolves")
}

fn written(target: &GizmoTarget, mode: GizmoMode, axis: GizmoAxis) -> Vec<ShapeWrite> {
    target.handle(mode, axis).unwrap().write.params().iter().map(|param| param.write.clone()).collect()
}

fn node_write(param: &str) -> ShapeWrite {
    ShapeWrite::Node(ParamAddr { scope_path: vec![9], node_doc_id: 77, param_id: param.to_string() })
}

/// The 640x480 view every helper projects through.
fn view() -> GizmoView {
    GizmoView::new(cam_looking_down_neg_z([0.0, 0.0, 0.0]), 640, 480)
}

/// The handle length at `point` in [`view`].
fn len_at(point: [f32; 3]) -> f32 {
    handle_length(point, &view().camera, 480)
}

fn px(point: [f32; 3]) -> (f32, f32) {
    let p = cam_looking_down_neg_z([0.0, 0.0, 0.0]).project_to_pixel(point, 640, 480).unwrap();
    (p.px, p.py)
}

#[test]
fn pick_target_finds_the_object_under_the_click() {
    let scene = scene_with(vec![known_object(1, (0.0, 0.0, -5.0), (false, false, false))]);
    let cam = cam_looking_down_neg_z([0.0, 0.0, 0.0]);
    assert_eq!(pick_target(&scene, &GizmoView::new(cam, 640, 480), px([0.0, 0.0, -5.0])), Some(object_id(1)));
    assert_eq!(pick_target(&scene, &GizmoView::new(cam, 640, 480), (5.0, 5.0)), None);
}

#[test]
fn object_target_derives_from_its_box_declaration() {
    let scene = scene_with(vec![known_object(7, (1.0, 2.0, 3.0), (false, false, false))]);
    let target = gizmo_target_for(&scene, &object_id(7)).expect("object 7 exists");
    assert_eq!(target.kind, GizmoTargetKind::Object);
    assert_eq!(target.origin, [1.0, 2.0, 3.0]);
    assert!([GizmoMode::Move, GizmoMode::Rotate, GizmoMode::Scale].into_iter().all(|mode| target.supports_mode(mode)));
    let ShapeWrite::Node(addr) = &written(&target, GizmoMode::Scale, GizmoAxis::Y)[0] else { panic!("node write") };
    assert_eq!(addr.param_id, "scale_y");
    assert!(gizmo_target_for(&scene, &object_id(42)).is_none());
}

#[test]
fn child_gizmo_composes_parent_origin_axes_and_drag_units() {
    let mut parent = known_object(7, (2.0, 3.0, -8.0), (false, false, false));
    let SceneObjectVm::Known(p) = &mut parent else { unreachable!() };
    p.is_group = true;
    let t = p.transform.as_mut().unwrap();
    t.rot_value.2 = std::f32::consts::FRAC_PI_2;
    t.scale_value = (2.0, 1.0, 1.0);
    let mut child = known_object(8, (1.0, 0.0, 0.0), (false, false, false));
    let SceneObjectVm::Known(c) = &mut child else { unreachable!() };
    c.parent_group_id = Some(7);
    let scene = scene_with(vec![parent, child]);
    let target = gizmo_target_for(&scene, &object_id(8)).unwrap();
    assert!((target.origin[0] - 2.0).abs() < 1e-5);
    assert!((target.origin[1] - 5.0).abs() < 1e-5);
    assert!(!target.pickable, "a child is selected through its parent");
    let start = px(target.origin);
    let end = px(offset(target.origin, target.axis_direction(GizmoAxis::X), 1.0));
    let HandleValues::One(value) = drag(&target, GizmoMode::Move, GizmoAxis::X, start, end) else { panic!("one value") };
    assert!((value - 2.0).abs() < 1e-4, "one local unit includes the parent's rotation and scale, got {value}");
}

#[test]
fn wired_param_shows_locked_and_refuses_the_drag() {
    let scene = scene_with(vec![known_object(1, (0.0, 0.0, -5.0), (true, false, false))]);
    let target = gizmo_target_for(&scene, &object_id(1)).unwrap();
    let lines = lines_of(GizmoMode::Move, &target);
    assert_eq!(lines.len(), 3);
    let x_line = lines.iter().find(|l| (l.b[0] - l.a[0]).abs() > 0.1).unwrap();
    assert_eq!(x_line.color, LOCKED_COLOR, "wired pos_x draws locked");
    let y_line = lines.iter().find(|l| (l.b[1] - l.a[1]).abs() > 0.1).unwrap();
    assert_eq!(y_line.color, Y_COLOR);
    assert!(GizmoDrag::begin(target.clone(), GizmoMode::Move, GizmoAxis::X, &view(), (0.0, 0.0)).is_none());
    assert!(GizmoDrag::begin(target, GizmoMode::Move, GizmoAxis::Y, &view(), (0.0, 0.0)).is_some());
}

#[test]
fn unwired_transform_target_is_unplaced_at_the_origin() {
    let mut row = known_object(5, (0.0, 0.0, 0.0), (false, false, false));
    let SceneObjectVm::Known(r) = &mut row else { unreachable!() };
    r.transform = None;
    let scene = scene_with(vec![row]);
    let target = gizmo_target_for(&scene, &object_id(5)).unwrap();
    assert_eq!(target.origin, [0.0, 0.0, 0.0]);
    assert!(target.unplaced);
    assert_eq!(written(&target, GizmoMode::Scale, GizmoAxis::Y), vec![ShapeWrite::Unplaced]);
    assert_eq!(lines_of(GizmoMode::Move, &target).len(), 3, "the declared handles still draw to grab");
    assert!(GizmoDrag::begin(target, GizmoMode::Move, GizmoAxis::X, &view(), (0.0, 0.0)).is_none(), "the host places it first");
}

#[test]
fn fluid_domain_targets_derive_from_the_domain_and_column_boxes() {
    let decls = scene_shape(GPU_FLIP_DOMAIN_TYPE_ID);
    let owner = ShapeOwner::LiquidDomain(NodeId::new("domain"));
    let domain = node_shape(owner.clone(), 0, &decls[0], &[("pos_y", 2.0), ("pos_z", -6.0), ("scale_x", 4.0), ("scale_y", 2.0), ("scale_z", 4.0)], &[]);
    let column = node_shape(owner.clone(), 1, &decls[1], &[("pos_x", -1.0), ("pos_z", -6.0), ("scale_x", 1.0)], &[]);
    let scene = scene_of(Vec::new(), vec![domain, column]);
    let targets: Vec<_> = gizmo_targets(&scene).collect();
    assert_eq!(targets.len(), 2, "a domain's box and column are two targets");
    let box_target = &targets[0];
    assert_eq!(box_target.kind, GizmoTargetKind::FluidDomain);
    assert_eq!(box_target.origin, [0.0, 2.0, -6.0]);
    assert!(!box_target.supports_mode(GizmoMode::Rotate), "the declaration gives the domain no rotation");
    assert!(lines_of(GizmoMode::Rotate, box_target).is_empty());
    assert_eq!(written(box_target, GizmoMode::Scale, GizmoAxis::X), vec![node_write("scale_x")]);
    let HandleValues::One(size) = drag(box_target, GizmoMode::Scale, GizmoAxis::X, px([1.0, 2.0, -6.0]), px([-20.0, 2.0, -6.0])) else {
        panic!("one value")
    };
    assert_eq!(size, DOMAIN_EXTENT_METRES.0, "a domain never scales below what the solvers accept");
    let HandleValues::One(size) = drag(box_target, GizmoMode::Scale, GizmoAxis::X, px([1.0, 2.0, -6.0]), px([20.0, 2.0, -6.0])) else {
        panic!("one value")
    };
    assert_eq!(size, DOMAIN_EXTENT_METRES.1, "nor above it");
    let HandleValues::One(column) = drag(&targets[1], GizmoMode::Scale, GizmoAxis::X, px([0.0, 0.0, -6.0]), px([-20.0, 0.0, -6.0])) else {
        panic!("one value")
    };
    assert_eq!(column, MIN_EXTENT, "an undeclared box stops only at the smallest extent");
    assert_eq!(targets[1].id, GizmoTargetId { owner, shape: 1 });
    assert_eq!(targets[1].origin, [-1.0, 0.0, -6.0]);
}

#[test]
fn role_source_targets_derive_their_kind_from_the_role() {
    let decl = &scene_shape("node.fluid_role_source")[0];
    for (role, kind) in [
        (FluidRole::Inflow, GizmoTargetKind::Inflow),
        (FluidRole::Outflow, GizmoTargetKind::Drain),
        (FluidRole::Collider, GizmoTargetKind::Collider),
    ] {
        let owner = ShapeOwner::RoleSource { node: NodeId::new("role"), role };
        let shape = node_shape(owner, 0, decl, &[("pos_x", 1.0), ("pos_z", -5.0)], &[]);
        let target = GizmoTarget::from_shape(&shape);
        assert_eq!(target.kind, kind);
        assert_eq!(target.origin, [1.0, 0.0, -5.0]);
        assert_eq!(written(&target, GizmoMode::Move, GizmoAxis::X), vec![node_write("pos_x")]);
        let HandleValues::One(x) = drag(&target, GizmoMode::Move, GizmoAxis::X, px([1.0, 0.0, -5.0]), px([2.0, 0.0, -5.0])) else {
            panic!("one value")
        };
        assert!((x - 2.0).abs() < 1e-4, "{kind:?} moved to {x}");
    }
}

#[test]
fn radial_force_target_derives_from_its_point_radius_declaration() {
    let decl = manifest_decl(r#"{"kind": "point_radius", "center": ["center_x", "center_y", "center_z"], "radius": "radius"}"#);
    let target = GizmoTarget::from_shape(&force_shape(decl, &[("center_z", -5.0), ("radius", 2.0)], [9.0; 3]));
    assert_eq!(target.kind, GizmoTargetKind::Force);
    assert_eq!(target.origin, [0.0, 0.0, -5.0], "a placed shape ignores the anchor");
    assert!(!target.supports_mode(GizmoMode::Rotate));
    let scale = &target.handles.iter().filter(|h| h.mode == GizmoMode::Scale).collect::<Vec<_>>();
    assert_eq!(scale.len(), 1, "one uniform radius handle");
    assert_eq!(written(&target, GizmoMode::Scale, GizmoAxis::Uniform), vec![ShapeWrite::Card { id: "host_radius".into(), scale: 1.0, offset: 0.0 }]);
    let along = |d: f32| px(offset(target.origin, GizmoAxis::Uniform.unit(), d));
    let HandleValues::One(radius) = drag(&target, GizmoMode::Scale, GizmoAxis::Uniform, along(1.0), along(2.0)) else {
        panic!("one value")
    };
    assert!((radius - 4.0).abs() < 1e-3, "twice as far from the centre doubles the radius, got {radius}");
    let cam = cam_looking_down_neg_z([0.0, 0.0, 0.0]);
    assert_eq!(pick_axis(GizmoMode::Scale, &target, &GizmoView::new(cam, 640, 480), px(offset(target.origin, GizmoAxis::Uniform.unit(), len_at(target.origin)))), Some(GizmoAxis::Uniform));
}

#[test]
fn vortex_force_rotate_turns_its_axis_and_keeps_its_length() {
    let decl = manifest_decl(
        r#"{"kind": "axis", "origin": ["center_x", "center_y", "center_z"], "direction": ["axis_x", "axis_y", "axis_z"], "radius": "radius"}"#,
    );
    let target = GizmoTarget::from_shape(&force_shape(decl, &[("center_z", -5.0), ("axis_x", 2.0), ("radius", 1.0)], [0.0; 3]));
    assert!(target.supports_mode(GizmoMode::Move) && target.supports_mode(GizmoMode::Scale));
    let ids: Vec<_> = written(&target, GizmoMode::Rotate, GizmoAxis::Z)
        .into_iter()
        .map(|write| match write {
            ShapeWrite::Card { id, .. } => id,
            ShapeWrite::Node(_) | ShapeWrite::Unplaced => panic!("card write"),
        })
        .collect();
    assert_eq!(ids, ["host_axis_x", "host_axis_y", "host_axis_z"]);
    let o = px(target.origin);
    // A quarter turn about Z, swept counter-clockwise on screen.
    let HandleValues::Three(axis) = drag(&target, GizmoMode::Rotate, GizmoAxis::Z, (o.0 + 100.0, o.1), (o.0, o.1 - 100.0)) else {
        panic!("three values")
    };
    let length = (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]).sqrt();
    assert!((length - 2.0).abs() < 1e-4, "length kept, got {axis:?}");
    assert!(axis[0].abs() < 0.1 && (axis[1].abs() - 2.0).abs() < 0.1, "turned a quarter, got {axis:?}");
    assert!(lines_of(GizmoMode::Rotate, &target).iter().any(|line| line.color == DIRECTION_COLOR));
}

#[test]
fn uniform_force_without_origin_sits_at_its_anchor() {
    let decl = manifest_decl(r#"{"kind": "axis", "direction": ["direction_x", "direction_y", "direction_z"]}"#);
    let shape = force_shape(decl, &[("direction_y", -1.0)], [0.5, 1.0, -4.0]);
    let scene = scene_of(Vec::new(), vec![shape]);
    let target = gizmo_targets(&scene).next().unwrap();
    assert_eq!(target.origin, [0.5, 1.0, -4.0]);
    assert!(!target.supports_mode(GizmoMode::Move), "no origin, no move handle");
    assert!(target.supports_mode(GizmoMode::Rotate));
    let cam = cam_looking_down_neg_z([0.0, 0.0, 0.0]);
    assert_eq!(pick_target(&scene, &GizmoView::new(cam, 640, 480), px([0.5, 1.0, -4.0])), Some(target.id.clone()));
}

#[test]
fn plane_rotate_writes_a_unit_normal() {
    let decl = SceneShapeDecl {
        shape: SceneShape::Plane {
            center: [Cow::Borrowed("cx"), Cow::Borrowed("cy"), Cow::Borrowed("cz")],
            normal: [Cow::Borrowed("nx"), Cow::Borrowed("ny"), Cow::Borrowed("nz")],
        },
        via: None,
        extent_limit: None,
    };
    let target = GizmoTarget::from_shape(&node_shape(ShapeOwner::Object(1), 0, &decl, &[("cz", -5.0), ("ny", 3.0)], &[]));
    let o = px(target.origin);
    let HandleValues::Three(n) = drag(&target, GizmoMode::Rotate, GizmoAxis::X, (o.0 + 80.0, o.1 + 10.0), (o.0 + 10.0, o.1 - 80.0)) else {
        panic!("three values")
    };
    assert!(((n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt() - 1.0).abs() < 1e-5, "{n:?}");
}

#[test]
fn pick_axis_returns_the_handle_under_the_click() {
    let scene = scene_with(vec![known_object(1, (0.0, 0.0, -5.0), (false, false, false))]);
    let target = gizmo_target_for(&scene, &object_id(1)).unwrap();
    let cam = cam_looking_down_neg_z([0.0, 0.0, 0.0]);
    let tip = offset(target.origin, GizmoAxis::X.unit(), len_at(target.origin));
    assert_eq!(pick_axis(GizmoMode::Move, &target, &GizmoView::new(cam, 640, 480), px(tip)), Some(GizmoAxis::X));
    assert_eq!(pick_axis(GizmoMode::Move, &target, &GizmoView::new(cam, 640, 480), (0.0, 0.0)), None);
}

#[test]
fn rotate_drag_delta_quarter_turn_is_roughly_half_pi() {
    let cam = cam_looking_down_neg_z([0.0, 0.0, 0.0]);
    let origin = [0.0, 0.0, -5.0];
    let o = cam.project_to_pixel(origin, 640, 480).unwrap();
    let d = rotate_drag_delta(origin, &cam, 640, 480, (o.px + 100.0, o.py), (o.px, o.py - 100.0)).unwrap();
    assert!((d.abs() - std::f32::consts::FRAC_PI_2).abs() < 0.05, "got {d}");
}

#[test]
fn ring_lines_are_closed_loop_of_24_segments() {
    let lines: Vec<_> = ring_lines([0.0, 0.0, 0.0], GizmoAxis::Y, 1.0, Y_COLOR).collect();
    assert_eq!(lines.len(), 24);
    for i in 0..3 {
        assert!((lines[0].a[i] - lines[23].b[i]).abs() < 1e-4);
    }
}

#[test]
fn scale_gizmo_lines_have_tick_marks() {
    let scene = scene_with(vec![known_object(1, (0.0, 0.0, 0.0), (false, false, false))]);
    let target = gizmo_target_for(&scene, &object_id(1)).unwrap();
    assert_eq!(lines_of(GizmoMode::Move, &target).len(), 3);
    assert_eq!(lines_of(GizmoMode::Scale, &target).len(), 6);
}

fn lines_of(mode: GizmoMode, target: &GizmoTarget) -> Vec<WorldLine> {
    let mut lines = Vec::new();
    gizmo_lines(mode, target, len_at(target.origin), &mut lines);
    lines
}

/// Grid, light markers, the accepted domain's box (a pending one draws
/// nothing), each physics shape, then the selected handles, faded when an
/// effect follows the scene.
#[test]
fn composition_draws_what_physics_did_then_the_selected_handles() {
    use manifold_core::fluid_domain::FluidDomainLayout;
    use manifold_core::physics_gizmo::{PhysicsGizmoFamily, PhysicsGizmoShape};
    let layout = FluidDomainLayout { min: [0.0; 3], size: [1.0; 3], cells: [8; 3], cell_size: 0.125 };
    let ready = FluidDomainSnapshot { epoch: 1, state: FluidDomainState::Ready, accepted_layout: Some(layout), fit: Default::default(), column_box: None, refusal: None };
    let pending = FluidDomainSnapshot { state: FluidDomainState::Initializing, ..ready.clone() };
    let domains = [(NodeId::new("a"), ready), (NodeId::new("b"), pending)];
    let physics = [
        PhysicsGizmo::aabb(PhysicsGizmoFamily::Collider, [0.0; 3], [1.0; 3]),
        PhysicsGizmo { family: PhysicsGizmoFamily::Force, shape: PhysicsGizmoShape::Arrow { from: [0.0; 3], to: [1.0, 0.0, 0.0] } },
    ];
    let scene = scene_with(vec![known_object(1, (0.0, 0.0, 0.0), (false, false, false))]);
    let target = gizmo_target_for(&scene, &object_id(1)).unwrap();
    let mut chrome = GizmoChrome {
        view: view(),
        lights: &[[0.0, 4.0, 0.0], [2.0, 1.0, -1.0]],
        domains: &domains,
        physics: &physics,
        problems: &[],
        selected: Some(&target),
        mode: GizmoMode::Move,
        fade_handles: false,
    };
    let mut lines = Vec::new();
    compose_lines(&chrome, &mut lines);
    let grid = GROUND_GRID.len();
    assert_eq!(lines.len(), grid + 2 * 3 + 12 + 12 + 5 + 3);
    let collider = crate::node_graph::viewport_overlay::physics_gizmo_color(PhysicsGizmoFamily::Collider);
    assert_eq!(lines[grid + 6 + 12].color, collider, "physics shapes follow the domain box");
    assert!(lines[lines.len() - 3..].iter().all(|line| line.color[3] == 255));

    chrome.fade_handles = true;
    compose_lines(&chrome, &mut lines);
    assert!(lines[lines.len() - 3..].iter().all(|line| line.color[3] == FADED_HANDLE_ALPHA), "handles fade");
    assert!(lines[..lines.len() - 3].iter().all(|line| line.color[3] == 255), "what physics did never fades");
}

/// B3: a shape the setup check finds outside its domain draws a red box in
/// every host's composed lines, and nothing else changes.
#[test]
fn a_setup_problem_draws_a_red_box_in_the_composed_lines() {
    use crate::node_graph::scene_vm::{SetupProblem, SetupProblemKind};
    let problem = SetupProblem {
        owner: ShapeOwner::LiquidDomain(NodeId::new("domain")),
        kind: SetupProblemKind::ColumnOutside,
        params: Vec::new(),
        bounds: Some(([5.0, 0.0, 0.0], [6.0, 1.0, 1.0])),
    };
    let compose = |problems: &[SetupProblem]| {
        let chrome = GizmoChrome {
            view: view(),
            lights: &[],
            domains: &[],
            physics: &[],
            problems,
            selected: None,
            mode: GizmoMode::Move,
            fade_handles: false,
        };
        let mut lines = Vec::new();
        compose_lines(&chrome, &mut lines);
        lines
    };
    let plain = compose(&[]);
    let unplaced = SetupProblem { bounds: None, ..problem.clone() };
    assert_eq!(compose(&[unplaced]).len(), plain.len(), "a problem nothing places draws no box");
    let flagged = compose(&[problem]);
    assert_eq!(flagged.len(), plain.len() + 12);
    assert!(
        flagged.iter().zip(&plain).all(|(a, b)| a.a == b.a && a.b == b.b && a.color == b.color),
        "the rest of the chrome is untouched"
    );
    let red = &flagged[plain.len()..];
    assert!(red.iter().all(|line| line.color == PROBLEM_COLOR));
    assert!(red.iter().flat_map(|line| [line.a, line.b]).all(|p| (5.0..=6.0).contains(&p[0]) && (0.0..=1.0).contains(&p[1])));
}

#[test]
fn each_grip_picks_its_own_handle() {
    let scene = scene_with(vec![known_object(1, (0.0, 0.0, -5.0), (false, false, false))]);
    let target = gizmo_target_for(&scene, &object_id(1)).unwrap();
    // Oblique, so no ring stands edge-on over another's grip.
    let cam = Camera::look_at([3.0, 2.5, 0.0], [0.0, 0.0, -5.0], [0.0, 1.0, 0.0], 0.9, 0.1, 100.0);
    let at = |p: [f32; 3]| cam.project_to_pixel(p, 640, 480).map(|p| (p.px, p.py)).unwrap();
    for mode in [GizmoMode::Move, GizmoMode::Scale, GizmoMode::Rotate] {
        for axis in GizmoAxis::XYZ {
            let grip = target.grip(mode, axis, &GizmoView::new(cam, 640, 480)).expect("an object has every handle");
            assert_eq!(pick_axis(mode, &target, &GizmoView::new(cam, 640, 480), at(grip)), Some(axis), "{mode:?} {axis:?}");
        }
    }
}

/// D-102: a handle spans the same share of the view at any distance, field
/// of view and view size, perspective or orthographic.
#[test]
fn handles_keep_their_screen_length_across_distance_and_field_of_view() {
    for height in [480_u32, 1080] {
        let width = height * 16 / 9;
        for fov in [0.3_f32, 0.9, 1.6] {
            for distance in [0.5_f32, 2.0, 30.0, 400.0, 5000.0] {
                let camera = Camera::look_at([0.0; 3], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0], fov, 0.1, 10_000.0);
                // Off centre too: the scale follows depth, not distance.
                let origin = [distance * 0.2, -distance * 0.1, -distance];
                let len = handle_length(origin, &camera, height);
                let a = camera.project_to_pixel(origin, width, height).unwrap();
                let b = camera.project_to_pixel(offset(origin, [1.0, 0.0, 0.0], len), width, height).unwrap();
                let on_screen = (b.px - a.px).hypot(b.py - a.py);
                let want = HANDLE_VIEW_FRACTION * height as f32;
                assert!((on_screen - want).abs() < want * 0.01, "{height}p fov {fov} at {distance}: {on_screen} px, want {want}");
            }
        }
    }
    let mut ortho = cam_looking_down_neg_z([0.0; 3]);
    ortho.mode = CameraMode::Orthographic { half_height: 40.0 };
    for distance in [1.0_f32, 90.0] {
        let origin = [0.0, 0.0, -distance];
        let a = ortho.project_to_pixel(origin, 640, 480).unwrap();
        let b = ortho.project_to_pixel(offset(origin, [0.0, 1.0, 0.0], handle_length(origin, &ortho, 480)), 640, 480).unwrap();
        assert!(((a.py - b.py) - HANDLE_VIEW_FRACTION * 480.0).abs() < 0.5, "ortho at {distance}");
    }
}

/// D-102: the drawn tip is where the press lands, near or far.
#[test]
fn a_press_on_the_drawn_tip_grabs_the_handle_at_any_distance() {
    for distance in [1.5_f32, 20.0, 300.0, 4000.0] {
        let scene = scene_with(vec![known_object(1, (0.0, 0.0, -distance), (false, false, false))]);
        let target = gizmo_target_for(&scene, &object_id(1)).unwrap();
        // Oblique, so no handle stands over another; the same direction at every distance.
        let eye = [3.0 * distance / 6.32, 2.5 * distance / 6.32, -distance + 5.0 * distance / 6.32];
        let camera = Camera::look_at(eye, [0.0, 0.0, -distance], [0.0, 1.0, 0.0], 0.9, 0.1, 10_000.0);
        let view = GizmoView::new(camera, 1280, 720);
        let chrome = GizmoChrome {
            view,
            lights: &[],
            domains: &[],
            physics: &[],
            problems: &[],
            selected: Some(&target),
            mode: GizmoMode::Move,
            fade_handles: false,
        };
        let mut lines = Vec::new();
        compose_lines(&chrome, &mut lines);
        let drawn = &lines[lines.len() - 3..];
        for (axis, line) in GizmoAxis::XYZ.into_iter().zip(drawn) {
            assert_eq!(Some(line.b), target.grip(GizmoMode::Move, axis, &view), "the grip is the drawn tip");
            let tip = camera.project_to_pixel(line.b, view.width, view.height).unwrap();
            assert_eq!(pick_axis(GizmoMode::Move, &target, &view, (tip.px, tip.py)), Some(axis), "{axis:?} at {distance}");
        }
    }
}

/// D-102: an object outlines its own render slot; an imported model's group
/// row outlines every part's; Water outlines its own slot and its looks'.
#[test]
fn outline_slots_cover_an_object_or_every_part_of_its_group() {
    let row = |id: u32, index: usize, group: Option<u32>, is_group: bool| {
        let SceneObjectVm::Known(mut row) = known_object(id, (0.0, 0.0, 0.0), (false, false, false)) else { unreachable!() };
        row.index = index;
        row.parent_group_id = group;
        row.is_group = is_group;
        SceneObjectVm::Known(row)
    };
    // An imported model's group row is a copy of its first part (scene_vm),
    // so it shares that part's slot. Water (20) is a real slot of its own,
    // with its looks (21, 22) as parts.
    let scene = scene_with(vec![
        row(1, 0, None, false),
        row(10, 1, None, true),
        row(11, 1, Some(10), false),
        row(12, 2, Some(10), false),
        row(20, 3, None, true),
        row(21, 4, Some(20), false),
        row(22, 5, Some(20), false),
        SceneObjectVm::Custom { index: 6 },
    ]);
    let mut slots = vec![99];
    outline_slots(&scene, 1, &mut slots);
    assert_eq!(slots, [0]);
    outline_slots(&scene, 10, &mut slots);
    assert_eq!(slots, [1, 2], "every part of the model, its shared slot once");
    outline_slots(&scene, 20, &mut slots);
    assert_eq!(slots, [3, 4, 5], "the water body and its looks");
    outline_slots(&scene, 7, &mut slots);
    assert!(slots.is_empty(), "no such object, no outline");
}

/// Hit tolerances hold in screen points on a 1x display, a 2x display and
/// a 4K output in a small preview: the same click in points picks the same
/// handle, and the same miss misses.
#[test]
fn hit_tolerances_hold_in_screen_points_at_any_display_scale() {
    let scene = scene_with(vec![known_object(1, (0.0, 0.0, -5.0), (false, false, false))]);
    let target = gizmo_target_for(&scene, &object_id(1)).unwrap();
    // (render height, the picture's height on screen in points)
    for (height, points_high) in [(480_u32, 480.0_f32), (960, 480.0), (2160, 458.0)] {
        let width = height * 16 / 9;
        let view = GizmoView::new(cam_looking_down_neg_z([0.0; 3]), width, height).on_screen(points_high);
        let s = view.px_per_point;
        let at = |p: [f32; 3]| {
            let p = view.camera.project_to_pixel(p, width, height).unwrap();
            (p.px, p.py)
        };
        let tip = at(view_tip(&target, &view, GizmoAxis::X));
        let below = |points: f32| (tip.0, tip.1 + points * s);
        assert_eq!(pick_axis(GizmoMode::Move, &target, &view, below(7.0)), Some(GizmoAxis::X), "{height}p: 7 pt off the tip");
        assert_eq!(pick_axis(GizmoMode::Move, &target, &view, below(14.0)), None, "{height}p: 14 pt off the tip");
        let origin = at(target.origin);
        let beside = |points: f32| (origin.0 - points * s, origin.1 + points * s);
        assert_eq!(pick_target(&scene, &view, beside(12.0)), Some(object_id(1)), "{height}p: 17 pt from the origin");
        assert_eq!(pick_target(&scene, &view, beside(20.0)), None, "{height}p: 28 pt from the origin");
    }
}

fn view_tip(target: &GizmoTarget, view: &GizmoView, axis: GizmoAxis) -> [f32; 3] {
    target.grip(GizmoMode::Move, axis, view).expect("a drawn handle")
}

/// A Move drag keeps the grabbed point under the cursor, as Blender and
/// Unity do: grab the tip and release on the origin's pixel and the object
/// moves back by exactly the handle's length; release three lengths out
/// and it moves two. Oblique perspective, near and far, every axis.
#[test]
fn a_move_drag_keeps_the_grabbed_point_under_the_cursor() {
    for distance in [3.0_f32, 40.0, 900.0] {
        let scene = scene_with(vec![known_object(1, (0.0, 0.0, -distance), (false, false, false))]);
        let target = gizmo_target_for(&scene, &object_id(1)).unwrap();
        let eye = [0.47 * distance, 0.4 * distance, -distance + 0.79 * distance];
        let camera = Camera::look_at(eye, [0.0, 0.0, -distance], [0.0, 1.0, 0.0], 0.9, 0.1, 10_000.0);
        let view = GizmoView::new(camera, 1280, 720);
        let at = |p: [f32; 3]| {
            let p = camera.project_to_pixel(p, 1280, 720).unwrap();
            (p.px, p.py)
        };
        let len = handle_length(target.origin, &camera, 720);
        for (i, axis) in GizmoAxis::XYZ.into_iter().enumerate() {
            let start = target.origin[i];
            let grabbed = view_tip(&target, &view, axis);
            let released = |to: [f32; 3]| {
                let mut drag = GizmoDrag::begin(target.clone(), GizmoMode::Move, axis, &view, at(grabbed)).unwrap();
                let HandleValues::One(v) = drag.update(&view, at(to)).expect("drag resolves") else { panic!("one value") };
                v - start
            };
            let back = released(target.origin);
            assert!((back + len).abs() < len * 0.01, "{axis:?} at {distance}: moved {back}, want {}", -len);
            let out = released(offset(target.origin, GizmoAxis::XYZ[i].unit(), 3.0 * len));
            assert!((out - 2.0 * len).abs() < len * 0.01, "{axis:?} at {distance}: moved {out}, want {}", 2.0 * len);
        }
    }
}

/// A handle pointing along the line of sight takes no press and has no
/// grip; the handles across the view still do.
#[test]
fn an_end_on_handle_takes_no_press() {
    let scene = scene_with(vec![known_object(1, (0.0, 0.0, -5.0), (false, false, false))]);
    let target = gizmo_target_for(&scene, &object_id(1)).unwrap();
    let view = view();
    assert!(target.grip(GizmoMode::Move, GizmoAxis::Z, &view).is_none(), "Z points at the camera");
    assert_ne!(pick_axis(GizmoMode::Move, &target, &view, px(target.origin)), Some(GizmoAxis::Z));
    assert!(target.grip(GizmoMode::Move, GizmoAxis::X, &view).is_some());
    assert!(target.grip(GizmoMode::Rotate, GizmoAxis::Z, &view).is_some(), "the ring facing the camera turns");
}

/// Dragging toward a receding axis's vanishing point pushes the object out
/// along the axis; at it and past it the drag holds, and the object never
/// lands behind the camera.
#[test]
fn a_drag_past_the_vanishing_point_holds_in_front_of_the_camera() {
    // The X axis recedes 25 degrees off the line of sight.
    let tilt = 25.0_f32.to_radians();
    let fwd = [tilt.cos(), 0.0, -tilt.sin()];
    let origin = fwd.map(|c| c * 5.0);
    let scene = scene_with(vec![known_object(1, (origin[0], origin[1], origin[2]), (false, false, false))]);
    let target = gizmo_target_for(&scene, &object_id(1)).unwrap();
    let camera = Camera::look_at([0.0; 3], origin, [0.0, 1.0, 0.0], 0.9, 0.1, 1000.0);
    let view = GizmoView::new(camera, 640, 480);
    // A pointer ray `degrees` off the line of sight, turned toward +X: the
    // part of +X across the line of sight.
    let side = normalized([1.0 - fwd[0] * fwd[0], 0.0, -fwd[0] * fwd[2]]).unwrap();
    let pointer = |degrees: f32| {
        let a = degrees.to_radians();
        let dir: [f32; 3] = std::array::from_fn(|i| fwd[i] * a.cos() + side[i] * a.sin());
        let p = camera.project_to_pixel(dir.map(|c| c * 10.0), 640, 480).unwrap();
        (p.px, p.py)
    };
    let grip = target.grip(GizmoMode::Move, GizmoAxis::X, &view).expect("the receding handle is drawn");
    let tip = camera.project_to_pixel(grip, 640, 480).unwrap();
    for degrees in [15.0_f32, 30.0, 40.0] {
        let mut drag = GizmoDrag::begin(target.clone(), GizmoMode::Move, GizmoAxis::X, &view, (tip.px, tip.py)).unwrap();
        match drag.update(&view, pointer(degrees)) {
            Some(HandleValues::One(x)) => {
                let moved = [x, origin[1], origin[2]];
                assert!(dot3(sub3(moved, camera.pos), camera.fwd) > camera.near, "{degrees} deg: the object lands behind the eye at {moved:?}");
                assert!(degrees < 25.0, "{degrees} deg is at or past the vanishing point and must hold, got x {x}");
                assert!(x > origin[0], "short of the vanishing point the object recedes along +X");
            }
            None => assert!(degrees >= 25.0, "{degrees} deg is short of the vanishing point and must move"),
            Some(values) => panic!("one value, got {values:?}"),
        }
    }
}

/// Scale multiplies by the ratio of axis distances, as Blender and Unity
/// scale: dragging the tip out to twice the handle's on-screen length
/// doubles the size, near or far.
#[test]
fn the_same_on_screen_scale_drag_scales_alike_near_and_far() {
    for distance in [3.0_f32, 40.0, 600.0] {
        let scene = scene_with(vec![known_object(1, (0.0, 0.0, -distance), (false, false, false))]);
        let target = gizmo_target_for(&scene, &object_id(1)).unwrap();
        let eye = [0.47 * distance, 0.4 * distance, -distance + 0.79 * distance];
        let camera = Camera::look_at(eye, [0.0, 0.0, -distance], [0.0, 1.0, 0.0], 0.9, 0.1, 10_000.0);
        let view = GizmoView::new(camera, 1280, 720);
        let at = |p: [f32; 3]| {
            let p = camera.project_to_pixel(p, 1280, 720).unwrap();
            (p.px, p.py)
        };
        let len = handle_length(target.origin, &camera, 720);
        let tip = view_tip(&target, &view, GizmoAxis::Y);
        let mut drag = GizmoDrag::begin(target.clone(), GizmoMode::Scale, GizmoAxis::Y, &view, at(tip)).unwrap();
        let HandleValues::One(size) = drag.update(&view, at(offset(target.origin, [0.0, 1.0, 0.0], 2.0 * len))).unwrap() else {
            panic!("one value")
        };
        assert!((size - 2.0).abs() < 0.01, "at {distance}: twice out doubles the size, got {size}");
    }
}

/// Under a scaled parent, handles still draw at the fixed on-screen length,
/// and a drag still moves the child in its own param units: a 3x parent
/// moves it a third as far per metre, a 0.2x parent five times as far.
#[test]
fn handles_under_a_scaled_parent_keep_their_size_and_param_units() {
    for parent_scale in [3.0_f32, 0.2] {
        let mut parent = known_object(7, (0.0, 0.0, -8.0), (false, false, false));
        let SceneObjectVm::Known(p) = &mut parent else { unreachable!() };
        p.is_group = true;
        p.transform.as_mut().unwrap().scale_value = (parent_scale, parent_scale, parent_scale);
        let mut child = known_object(8, (0.5, 0.0, 0.0), (false, false, false));
        let SceneObjectVm::Known(c) = &mut child else { unreachable!() };
        c.parent_group_id = Some(7);
        let scene = scene_with(vec![parent, child]);
        let target = gizmo_target_for(&scene, &object_id(8)).unwrap();
        let camera = Camera::look_at([2.0, 1.5, 0.0], target.origin, [0.0, 1.0, 0.0], 0.9, 0.1, 100.0);
        let view = GizmoView::new(camera, 640, 480);
        let at = |p: [f32; 3]| {
            let p = camera.project_to_pixel(p, 640, 480).unwrap();
            (p.px, p.py)
        };
        let len = handle_length(target.origin, &camera, 480);
        let mut lines = Vec::new();
        gizmo_lines(GizmoMode::Move, &target, len, &mut lines);
        let x_line = lines.iter().find(|line| line.color == X_COLOR).expect("the X handle");
        let d = sub3(x_line.b, x_line.a);
        let world_len = dot3(d, d).sqrt();
        assert!((world_len - len).abs() < len * 1e-3, "parent {parent_scale}x: the handle is the fixed length, got {world_len} of {len}");
        let tip = view_tip(&target, &view, GizmoAxis::X);
        assert_eq!(pick_axis(GizmoMode::Move, &target, &view, at(tip)), Some(GizmoAxis::X));
        let mut drag = GizmoDrag::begin(target.clone(), GizmoMode::Move, GizmoAxis::X, &view, at(tip)).unwrap();
        let HandleValues::One(x) = drag.update(&view, at(target.origin)).unwrap() else { panic!("one value") };
        let want = 0.5 - len / parent_scale;
        assert!((x - want).abs() < len * 0.01 / parent_scale, "parent {parent_scale}x: pos_x {x}, want {want}");
    }
}

/// The grab is fixed at the press: if the camera moves mid-drag (an
/// animated show camera), releasing on the origin as the picture now shows
/// it still moves the object back by the handle length it was grabbed at.
#[test]
fn a_camera_moving_mid_drag_never_shifts_the_grab() {
    let scene = scene_with(vec![known_object(1, (0.0, 0.0, -10.0), (false, false, false))]);
    let target = gizmo_target_for(&scene, &object_id(1)).unwrap();
    let pressed = GizmoView::new(Camera::look_at([3.0, 2.0, -2.0], [0.0, 0.0, -10.0], [0.0, 1.0, 0.0], 0.9, 0.1, 100.0), 640, 480);
    let moved = GizmoView::new(Camera::look_at([-4.0, 3.0, -1.0], [0.0, 0.0, -10.0], [0.0, 1.0, 0.0], 0.9, 0.1, 100.0), 640, 480);
    let at = |view: &GizmoView, p: [f32; 3]| {
        let p = view.camera.project_to_pixel(p, 640, 480).unwrap();
        (p.px, p.py)
    };
    let len = handle_length(target.origin, &pressed.camera, 480);
    let tip = view_tip(&target, &pressed, GizmoAxis::X);
    let mut drag = GizmoDrag::begin(target.clone(), GizmoMode::Move, GizmoAxis::X, &pressed, at(&pressed, tip)).unwrap();
    let HandleValues::One(x) = drag.update(&moved, at(&moved, target.origin)).unwrap() else { panic!("one value") };
    assert!((x + len).abs() < len * 0.01, "moved back by the grabbed length {len}, got {x}");
}

fn domain_chrome_lines(layout: manifold_core::fluid_domain::FluidDomainLayout, view: GizmoView, selected: bool) -> (Vec<WorldLine>, usize) {
    use manifold_core::fluid_domain::{FluidDomainSnapshot, FluidDomainState};
    let decls = scene_shape(GPU_FLIP_DOMAIN_TYPE_ID);
    let owner = ShapeOwner::LiquidDomain(NodeId::new("domain"));
    let shape = node_shape(owner.clone(), 0, &decls[0], &[("scale_x", 4.0), ("scale_y", 2.0), ("scale_z", 4.0)], &[]);
    let scene = scene_of(Vec::new(), vec![shape]);
    let target = gizmo_target_for(&scene, &GizmoTargetId { owner, shape: 0 }).unwrap();
    let domains = [(NodeId::new("domain"), FluidDomainSnapshot { epoch: 1, state: FluidDomainState::Ready, accepted_layout: Some(layout), fit: Default::default(), column_box: None, refusal: None })];
    let chrome = GizmoChrome {
        view,
        lights: &[],
        domains: &domains,
        physics: &[],
        problems: &[],
        selected: selected.then_some(&target),
        mode: GizmoMode::Move,
        fade_handles: false,
    };
    let mut lines = Vec::new();
    compose_lines(&chrome, &mut lines);
    let handles = if selected { lines_of(GizmoMode::Move, &target).len() } else { 0 };
    let base = GROUND_GRID.len() + 12;
    let lattice = lines.len() - base - handles;
    (lines, lattice)
}

fn is_lattice(line: &WorldLine) -> bool {
    line.color == [230, 200, 60, 70]
}

/// A3: the selected domain draws the solver's own cells, only while selected.
#[test]
fn selected_domain_draws_cell_boundaries_of_the_reported_layout() {
    use manifold_core::fluid_domain::{DomainBox, domain_layout};
    let pose = DomainBox { pos: [0.0, 2.0, 0.0], rot_euler: [0.0; 3], scale: [4.0, 2.0, 4.0], billboard: false };
    let layout = domain_layout(Some(pose), 4.0, 64).unwrap();
    let eye = Camera::look_at([30.0, 20.0, 40.0], [0.0, 2.0, 0.0], [0.0, 1.0, 0.0], 0.9, 0.1, 500.0);
    let view = GizmoView::new(eye, 1920, 1080);
    let (_, none) = domain_chrome_lines(layout, view, false);
    assert_eq!(none, 0, "an unselected domain draws no cell grid");
    let (lines, count) = domain_chrome_lines(layout, view, true);
    assert!(count > 0);
    let cell = layout.cell_size as f32;
    for line in lines.iter().filter(|l| is_lattice(l)) {
        let along = (0..3).find(|&k| (line.a[k] - line.b[k]).abs() > 1e-6).unwrap();
        let mut fixed = (0..3).filter(|&k| k != along);
        // One fixed axis is the face (a box wall), the other a cell boundary.
        let on_boundary = |k: usize| {
            let steps = (line.a[k] - layout.min[k]) / cell;
            (steps - steps.round()).abs() < 1e-3
        };
        assert!(fixed.all(on_boundary), "off the solver's cells: {line:?}");
    }
}

/// A3: which faces draw. Outside the box the face away from the eye; inside
/// it the wall the camera looks toward.
#[test]
fn lattice_faces_are_far_outside_and_looked_at_inside() {
    use manifold_core::fluid_domain::FluidDomainLayout;
    let layout = FluidDomainLayout { min: [-10.0, 0.5, -10.0], size: [20.0, 0.5, 20.0], cells: [320, 8, 320], cell_size: 0.0625 };
    let outside = crate::node_graph::viewport_overlay::lattice_faces(&layout, [30.0, 5.0, -40.0], [-0.5, -0.1, 0.8]);
    assert_eq!(outside, [-10.0, 0.5, 10.0], "away from an eye at +x, above, -z");
    let inside_toward_max = crate::node_graph::viewport_overlay::lattice_faces(&layout, [1.0, 0.7, 2.0], [0.6, 0.0, 0.8]);
    assert_eq!(inside_toward_max, [10.0, 1.0, 10.0], "the walls ahead");
    let inside_toward_min = crate::node_graph::viewport_overlay::lattice_faces(&layout, [1.0, 0.7, 2.0], [-0.6, -0.2, -0.8]);
    assert_eq!(inside_toward_min, [-10.0, 0.5, -10.0]);
}

/// A3: past 257 cells on an axis the stride runs: the count stays in budget
/// and neighbours sit a whole number of cells apart.
#[test]
fn a_wide_domain_strides_within_the_line_budget() {
    use crate::node_graph::viewport_overlay::{LATTICE_LINE_BUDGET, fluid_lattice_lines};
    use manifold_core::fluid_domain::{DomainBox, domain_layout};
    let pose = DomainBox { pos: [0.0, 4.0, 0.0], rot_euler: [0.0; 3], scale: [20.0, 0.5, 20.0], billboard: false };
    let layout = domain_layout(Some(pose), 20.0, 1280).unwrap();
    assert!(layout.cells[0] > 257, "{:?}", layout.cells);
    let mut lines = Vec::new();
    // No screen thinning: the cap alone.
    fluid_lattice_lines(layout, [60.0, 40.0, 60.0], [-0.6, -0.4, -0.6], |_, _| None, &mut lines);
    assert!(!lines.is_empty() && lines.len() <= LATTICE_LINE_BUDGET, "{} lines", lines.len());
    let cell = layout.cell_size as f32;
    let stride = layout.cells[0].saturating_sub(1).div_ceil(256);
    assert!(stride > 1);
    let mut xs: Vec<f32> = lines.iter().filter(|l| l.a[0] == l.b[0] && l.a[1] == l.b[1] && l.a[1] == layout.min[1]).map(|l| l.a[0]).collect();
    xs.sort_by(f32::total_cmp);
    assert!(xs.windows(2).all(|w| ((w[1] - w[0]) / cell - stride as f32).abs() < 1e-2), "stride {stride}");
}

/// A3: a 256-cell domain from a typical distance keeps neighbouring lines at
/// least the chosen spacing apart on screen, and a floor on the ground plane
/// draws no face over the ground grid.
#[test]
fn a_dense_domain_is_thinned_to_the_screen_spacing() {
    use crate::node_graph::viewport_overlay::LATTICE_MIN_POINTS;
    use manifold_core::fluid_domain::{DomainBox, domain_layout};
    let pose = DomainBox { pos: [0.0, 3.0, 0.0], rot_euler: [0.0; 3], scale: [6.0, 4.0, 6.0], billboard: false };
    let layout = domain_layout(Some(pose), 6.0, 256).unwrap();
    let cam = Camera::look_at([9.0, 7.0, 13.0], [0.0, 2.0, 0.0], [0.0, 1.0, 0.0], 0.8, 0.1, 200.0);
    let view = GizmoView::new(cam, 1280, 720);
    let (lines, count) = domain_chrome_lines(layout, view, true);
    assert!(count > 0 && count <= crate::node_graph::viewport_overlay::LATTICE_LINE_BUDGET);
    let lattice: Vec<&WorldLine> = lines.iter().filter(|l| is_lattice(l)).collect();
    // Group parallel lines of one face; consecutive ones are neighbours.
    let mut groups: std::collections::BTreeMap<(usize, usize, i64), Vec<(f32, f32)>> = Default::default();
    for l in &lattice {
        let along = (0..3).find(|&k| (l.a[k] - l.b[k]).abs() > 1e-6).unwrap();
        let face = (0..3).find(|&k| k != along && (l.a[k] == layout.min[k] || l.a[k] == layout.min[k] + layout.size[k])).unwrap();
        let across = 3 - along - face;
        let mid = [0, 1, 2].map(|k| (l.a[k] + l.b[k]) * 0.5);
        let p = cam.project_to_pixel(mid, 1280, 720).unwrap();
        groups.entry((along, face, (l.a[face] * 1000.0) as i64)).or_default().push((l.a[across], p.px));
        let _ = p.py;
    }
    assert!(groups.len() >= 4, "grid faces drawn: {}", groups.len());
    for ((along, face, _), mut v) in groups {
        v.sort_by(|a, b| a.0.total_cmp(&b.0));
        for w in v.windows(2) {
            let (p0, p1) = (pt(&cam, along, face, &layout, w[0].0), pt(&cam, along, face, &layout, w[1].0));
            let gap = (p0.0 - p1.0).hypot(p0.1 - p1.1);
            assert!(gap >= LATTICE_MIN_POINTS * 0.9, "lines {gap} px apart (axis {along}, face {face})");
        }
    }
    // A floor resting on y = 0 skips its face.
    let on_ground = manifold_core::fluid_domain::FluidDomainLayout { min: [-3.0, 0.0, -3.0], size: [6.0, 4.0, 6.0], cells: [64, 43, 64], cell_size: 6.0 / 64.0 };
    let top = Camera::look_at([0.0, 40.0, 0.1], [0.0, 0.0, 0.0], [0.0, 1.0, 0.0], 0.8, 0.1, 200.0);
    let (floor_lines, _) = domain_chrome_lines(on_ground, GizmoView::new(top, 1280, 720), true);
    assert!(
        floor_lines.iter().filter(|l| is_lattice(l)).all(|l| l.a[1] != l.b[1] || l.a[1] != 0.0),
        "no cell lines lie flat on the ground plane"
    );
}

/// Pixel position of the midpoint of the line across `across`=`value` on the
/// given face, for the spacing check.
fn pt(cam: &Camera, along: usize, face: usize, layout: &manifold_core::fluid_domain::FluidDomainLayout, value: f32) -> (f32, f32) {
    let across = 3 - along - face;
    let mut mid = [0.0; 3];
    mid[along] = layout.min[along] + layout.size[along] * 0.5;
    mid[across] = value;
    mid[face] = if cam.pos[face] < layout.min[face] + layout.size[face] * 0.5 { layout.min[face] + layout.size[face] } else { layout.min[face] };
    let p = cam.project_to_pixel(mid, 1280, 720).unwrap();
    (p.px, p.py)
}
