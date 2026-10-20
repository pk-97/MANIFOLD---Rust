use std::borrow::Cow;

use manifold_core::NodeId;
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
        look_mesh: None,
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
        fluid_controls: Vec::new(),
        fluid_domain: None,
        fluid_domain_transform: None,
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
    let mut drag = GizmoDrag::begin(target.clone(), mode, axis, from).expect("handle is draggable");
    drag.update(&cam, 640, 480, to).expect("drag resolves")
}

fn written(target: &GizmoTarget, mode: GizmoMode, axis: GizmoAxis) -> Vec<ShapeWrite> {
    target.handle(mode, axis).unwrap().write.params().iter().map(|param| param.write.clone()).collect()
}

fn node_write(param: &str) -> ShapeWrite {
    ShapeWrite::Node(ParamAddr { scope_path: vec![9], node_doc_id: 77, param_id: param.to_string() })
}

fn px(point: [f32; 3]) -> (f32, f32) {
    let p = cam_looking_down_neg_z([0.0, 0.0, 0.0]).project_to_pixel(point, 640, 480).unwrap();
    (p.px, p.py)
}

#[test]
fn pick_target_finds_the_object_under_the_click() {
    let scene = scene_with(vec![known_object(1, (0.0, 0.0, -5.0), (false, false, false))]);
    let cam = cam_looking_down_neg_z([0.0, 0.0, 0.0]);
    assert_eq!(pick_target(&scene, &cam, 640, 480, px([0.0, 0.0, -5.0])), Some(object_id(1)));
    assert_eq!(pick_target(&scene, &cam, 640, 480, (5.0, 5.0)), None);
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
    let scene = scene_with(vec![known_object(1, (0.0, 0.0, 0.0), (true, false, false))]);
    let target = gizmo_target_for(&scene, &object_id(1)).unwrap();
    let lines = lines_of(GizmoMode::Move, &target);
    assert_eq!(lines.len(), 3);
    let x_line = lines.iter().find(|l| (l.b[0] - l.a[0]).abs() > 0.5).unwrap();
    assert_eq!(x_line.color, LOCKED_COLOR, "wired pos_x draws locked");
    let y_line = lines.iter().find(|l| (l.b[1] - l.a[1]).abs() > 0.5).unwrap();
    assert_eq!(y_line.color, Y_COLOR);
    assert!(GizmoDrag::begin(target.clone(), GizmoMode::Move, GizmoAxis::X, (0.0, 0.0)).is_none());
    assert!(GizmoDrag::begin(target, GizmoMode::Move, GizmoAxis::Y, (0.0, 0.0)).is_some());
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
    assert!(GizmoDrag::begin(target, GizmoMode::Move, GizmoAxis::X, (0.0, 0.0)).is_none(), "the host places it first");
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
    let HandleValues::One(size) = drag(box_target, GizmoMode::Scale, GizmoAxis::X, px([0.0, 2.0, -6.0]), px([-50.0, 2.0, -6.0])) else {
        panic!("one value")
    };
    assert_eq!(size, MIN_EXTENT, "a box never scales below its smallest size");
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
    let tip = offset(target.origin, GizmoAxis::Uniform.unit(), 1.0);
    let HandleValues::One(radius) = drag(&target, GizmoMode::Scale, GizmoAxis::Uniform, px(target.origin), px(tip)) else {
        panic!("one value")
    };
    assert!((radius - 3.0).abs() < 1e-3, "one unit along the handle grows the radius by one, got {radius}");
    let cam = cam_looking_down_neg_z([0.0, 0.0, 0.0]);
    assert_eq!(pick_axis(GizmoMode::Scale, &target, &cam, 640, 480, px(offset(target.origin, GizmoAxis::Uniform.unit(), GIZMO_HANDLE_LEN))), Some(GizmoAxis::Uniform));
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
    assert_eq!(pick_target(&scene, &cam, 640, 480, px([0.5, 1.0, -4.0])), Some(target.id.clone()));
}

#[test]
fn plane_rotate_writes_a_unit_normal() {
    let decl = SceneShapeDecl {
        shape: SceneShape::Plane {
            center: [Cow::Borrowed("cx"), Cow::Borrowed("cy"), Cow::Borrowed("cz")],
            normal: [Cow::Borrowed("nx"), Cow::Borrowed("ny"), Cow::Borrowed("nz")],
        },
        via: None,
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
    let tip = offset(target.origin, GizmoAxis::X.unit(), GIZMO_HANDLE_LEN);
    assert_eq!(pick_axis(GizmoMode::Move, &target, &cam, 640, 480, px(tip)), Some(GizmoAxis::X));
    assert_eq!(pick_axis(GizmoMode::Move, &target, &cam, 640, 480, (0.0, 0.0)), None);
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
    gizmo_lines(mode, target, &mut lines);
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
    let ready = FluidDomainSnapshot { epoch: 1, state: FluidDomainState::Ready, accepted_layout: Some(layout) };
    let pending = FluidDomainSnapshot { state: FluidDomainState::Initializing, ..ready };
    let domains = [(NodeId::new("a"), ready), (NodeId::new("b"), pending)];
    let physics = [
        PhysicsGizmo::aabb(PhysicsGizmoFamily::Collider, [0.0; 3], [1.0; 3]),
        PhysicsGizmo { family: PhysicsGizmoFamily::Force, shape: PhysicsGizmoShape::Arrow { from: [0.0; 3], to: [1.0, 0.0, 0.0] } },
    ];
    let scene = scene_with(vec![known_object(1, (0.0, 0.0, 0.0), (false, false, false))]);
    let target = gizmo_target_for(&scene, &object_id(1)).unwrap();
    let mut chrome = GizmoChrome {
        lights: &[[0.0, 4.0, 0.0], [2.0, 1.0, -1.0]],
        domains: &domains,
        physics: &physics,
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

#[test]
fn each_grip_picks_its_own_handle() {
    let scene = scene_with(vec![known_object(1, (0.0, 0.0, -5.0), (false, false, false))]);
    let target = gizmo_target_for(&scene, &object_id(1)).unwrap();
    // Oblique, so no ring stands edge-on over another's grip.
    let cam = Camera::look_at([3.0, 2.5, 0.0], [0.0, 0.0, -5.0], [0.0, 1.0, 0.0], 0.9, 0.1, 100.0);
    let at = |p: [f32; 3]| cam.project_to_pixel(p, 640, 480).map(|p| (p.px, p.py)).unwrap();
    for mode in [GizmoMode::Move, GizmoMode::Scale, GizmoMode::Rotate] {
        for axis in GizmoAxis::XYZ {
            let grip = target.grip(mode, axis).expect("an object has every handle");
            assert_eq!(pick_axis(mode, &target, &cam, 640, 480, at(grip)), Some(axis), "{mode:?} {axis:?}");
        }
    }
}
