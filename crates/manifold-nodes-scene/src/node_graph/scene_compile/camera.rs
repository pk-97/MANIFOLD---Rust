//! Camera: a rig and its lens per Camera item. The active camera (the
//! World's choice, else the first camera) drives the render and the
//! cinematic tail after it: depth of field, then motion blur, then out.

use manifold_core::NodeId;
use manifold_core::effect_graph_def::EffectGraphNode;
use manifold_core::scene_doc::{CameraRig, Item, ItemFields, ItemId};

use super::builder::Builder;
use crate::node_graph::cinematic_tail::build_cinematic_tail;

/// Emit every camera in tree order, wire the active one, then each camera's
/// rows (the active camera's include its depth of field and motion blur).
pub(crate) fn emit_all(b: &mut Builder<'_>) {
    let mut cameras = Vec::new();
    b.doc.walk(|item, _| {
        if let ItemFields::Camera(_) = item.fields {
            cameras.push(item.clone());
        }
    });
    for item in &cameras {
        emit(b, item);
    }
    let chosen = b
        .doc
        .world()
        .and_then(|world| match &world.fields {
            ItemFields::World(w) => w.active_camera.clone(),
            _ => None,
        })
        .filter(|id| cameras.iter().any(|camera| &camera.id == id))
        .or_else(|| cameras.first().map(|camera| camera.id.clone()));
    if let Some(active) = chosen {
        wire_active(b, &active);
    }
    for item in &cameras {
        b.rows(item);
    }
}

fn emit(b: &mut Builder<'_>, item: &Item) {
    let ItemFields::Camera(fields) = &item.fields else {
        return;
    };
    let owner = item.id.as_str();
    let rig_type = match fields.rig {
        CameraRig::Orbit => "node.orbit_camera",
        CameraRig::Free => "node.free_camera",
        CameraRig::LookAt => "node.look_at_camera",
    };
    let rig = b.add(owner, "rig", rig_type);
    let lens = b.add(owner, "lens", "node.camera_lens");
    b.wire(rig, "out", lens, "camera");
    b.lenses.push(lens);
}

/// The active camera's lens feeds the render and the tail.
fn wire_active(b: &mut Builder<'_>, camera: &ItemId) {
    let owner = camera.as_str();
    let lens = b.id(owner, "lens").expect("camera lens");
    let render = b.scene("render");
    b.wire(lens, "out", render, "camera");
    let tail = build_cinematic_tail(&mut || b.fresh(), 1.0, false);
    for mut node in tail.nodes {
        rename(&mut node, camera);
        if let Some(coc) = find_type(&node, "node.coc_from_depth") {
            b.adopt(owner, "coc", coc);
        }
        b.push_node(node);
    }
    b.adopt(owner, "bokeh", tail.bokeh_id);
    b.adopt(owner, "motion_blur", tail.motion_blur_id);
    let dof = tail.dof_group_id;
    let blur = tail.motion_blur_id;
    b.wire(render, "depth", dof, "depth");
    b.wire(lens, "out", dof, "camera");
    b.wire(render, "color", dof, "color");
    b.wire(dof, "out", blur, "in");
    b.wire(render, "velocity", blur, "velocity");
    b.wire(lens, "out", blur, "camera");
    b.tail = Some(blur);
    b.active_lens = Some(lens);
}

/// Give the tail's nodes ids from the camera, inside its group too, so two
/// cameras' tails never share one and a recompile keeps them.
fn rename(node: &mut EffectGraphNode, camera: &ItemId) {
    node.node_id = NodeId::new(format!("{camera}:{}", node.node_id));
    node.handle = Some(node.node_id.to_string());
    if let Some(body) = node.group.as_deref_mut() {
        body.nodes.iter_mut().for_each(|inner| rename(inner, camera));
    }
}

fn find_type(node: &EffectGraphNode, type_id: &str) -> Option<u32> {
    if node.type_id == type_id {
        return Some(node.id);
    }
    node.group.as_deref()?.nodes.iter().find_map(|inner| find_type(inner, type_id))
}

/// Aim each Look At camera that has a Target item at that item's drawn frame:
/// a body's solved pose, else its transform. Runs after the tree has emitted
/// every frame.
pub(crate) fn aim_all(b: &mut Builder<'_>) {
    let mut aims = Vec::new();
    b.doc.walk(|item, _| {
        if let ItemFields::Camera(camera) = &item.fields
            && camera.rig == CameraRig::LookAt
            && let Some(target) = &camera.target
        {
            aims.push((item.id.clone(), target.clone()));
        }
    });
    for (camera, target) in aims {
        let frame = b.id(target.as_str(), "pose_frame").or_else(|| b.id(target.as_str(), "transform"));
        if let (Some(rig), Some(frame)) = (b.id(camera.as_str(), "rig"), frame) {
            b.wire(frame, "transform", rig, "target");
        }
    }
}
