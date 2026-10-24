//! Camera: a rig, its shake and its lens per Camera item, and the live cut
//! between them (ruling 11). Every camera compiles; a camera switch feeds the
//! render the one the World's Camera row picks. The row's value is the
//! camera's cut number (`CameraFields::cut`), and switch input k carries the
//! camera holding number k, so a recorded cut keeps its camera whatever the
//! tree does. Each camera has its own depth of field and motion blur after
//! the render, and an image switch on the same pick shows that camera's, so
//! only the picked camera's run. A cut is a param write: it lands on its
//! frame and never recompiles. Camera names never reach the graph: the panel
//! labels the row from the document, so a rename recompiles nothing.

use manifold_core::NodeId;
use manifold_core::effect_graph_def::{EffectGraphNode, SerializedParamValue};
use manifold_core::scene_doc::{CAMERA_PICK, CameraRig, Item, ItemFields, ItemId, param_id};

use super::builder::{Builder, SCENE};
use crate::node_graph::cinematic_tail::build_cinematic_tail;
use crate::node_graph::primitives::camera_switch::camera_switch_input;

/// A Loop camera's loop length in beats until a Scene Loop sets it.
pub(crate) const LOOP_BEATS: f32 = 8.0;

/// Emit every camera in cut-number order, the cut between them, then each
/// camera's rows.
pub(crate) fn emit_all(b: &mut Builder<'_>) {
    let doc = b.doc;
    let cameras: Vec<Item> = doc
        .cuts()
        .into_iter()
        .filter_map(|(_, id)| doc.find(id).cloned())
        .collect();
    if cameras.is_empty() {
        return;
    }
    let slots = cut_slots(doc.cut_count(), &cameras);
    let lenses: Vec<u32> = cameras.iter().map(|item| emit(b, item)).collect();
    let owner = cut_owner(b);
    let switch = b.add(&owner, "camera_switch", "node.camera_switch");
    b.set(
        switch,
        "num_inputs",
        SerializedParamValue::Int {
            value: slots.len() as i32,
        },
    );
    b.set(switch, "select", SerializedParamValue::Enum { value: 0 });
    for (k, &camera) in slots.iter().enumerate() {
        b.wire(lenses[camera], "out", switch, &camera_switch_input(k));
    }
    let render = b.scene("render");
    b.wire(switch, "out", render, "camera");
    b.active_lens = Some(switch);

    let tails: Vec<u32> = cameras
        .iter()
        .zip(&lenses)
        .map(|(item, &lens)| tail(b, &item.id, lens))
        .collect();
    b.tail = Some(match tails.as_slice() {
        [only] => *only,
        _ => {
            let image = b.add(&owner, "camera_cut", "node.switch_texture");
            b.set(
                image,
                "num_inputs",
                SerializedParamValue::Int {
                    value: slots.len() as i32,
                },
            );
            b.set(
                image,
                "selector",
                SerializedParamValue::Float { value: 0.0 },
            );
            for (k, &camera) in slots.iter().enumerate() {
                b.wire(tails[camera], "out", image, &format!("in_{k}"));
            }
            image
        }
    });
    for item in &cameras {
        b.rows(item);
    }
}

/// Which camera (an index into `cameras`, lowest cut first) each switch
/// input carries: input k carries the camera holding cut number k, and a
/// number no camera holds carries the first camera. One input past the last
/// number carries the first camera too, since both switches hold a pick past
/// their inputs at the last one.
fn cut_slots(count: u32, cameras: &[Item]) -> Vec<usize> {
    let mut slots = vec![0; count as usize + 1];
    // Backwards, so of two cameras holding one number the first wins.
    for (k, item) in cameras.iter().enumerate().rev() {
        if let ItemFields::Camera(camera) = &item.fields
            && let Some(slot) = slots.get_mut(camera.cut as usize)
        {
            *slot = k;
        }
    }
    slots
}

/// After the World's rows: the image switch follows the Camera row, and the
/// row spans every cut number.
pub(crate) fn finish_cut(b: &mut Builder<'_>) {
    let owner = cut_owner(b);
    let Some(switch) = b.id(&owner, "camera_switch") else {
        return;
    };
    if let Some(image) = b.id(&owner, "camera_cut") {
        b.follow(switch, "select", image, "selector");
    }
    let doc = b.doc;
    let Some(world) = doc.world() else {
        return;
    };
    let last = doc.cut_count().saturating_sub(1);
    b.pick_range(&param_id(&world.id, CAMERA_PICK), last as f32);
}

/// The cut belongs to the World, whose card has the Camera row.
fn cut_owner(b: &Builder<'_>) -> String {
    b.doc
        .world()
        .map_or_else(|| SCENE.to_owned(), |world| world.id.to_string())
}

/// The camera's rig, shaken about its own position, then its lens. A group
/// the camera sits in moves the rig; the shake rides on top of wherever the
/// rig ends up. Returns the lens.
fn emit(b: &mut Builder<'_>, item: &Item) -> u32 {
    let owner = item.id.as_str();
    let rig_type = match &item.fields {
        ItemFields::Camera(fields) => match fields.rig {
            CameraRig::Orbit => "node.orbit_camera",
            CameraRig::Free => "node.free_camera",
            CameraRig::LookAt => "node.look_at_camera",
            CameraRig::Loop => "node.loop_camera",
        },
        _ => "node.orbit_camera",
    };
    let rig = b.add(owner, "rig", rig_type);
    if rig_type == "node.loop_camera" {
        // The loop's clock: one ramp per loop of Beats, on the playback beat
        // (the ramp reads it itself; the sealed scene takes no layer input).
        // The scene's Scene Loop drives Beats and the corridor params
        // (`scene_loop.rs`); with none, the camera flies the node's own
        // corridor every LOOP_BEATS beats.
        let phase = b.add(owner, "phase", "node.beat_ramp");
        b.set_number(phase, "bars", LOOP_BEATS);
        b.wire(phase, "out", rig, "phase");
    }
    let shake = b.add(owner, "shake", "node.transform_shake");
    let lens = b.add(owner, "lens", "node.camera_lens");
    b.wire(rig, "out", shake, "camera");
    b.wire(shake, "camera_out", lens, "camera");
    b.lenses.push(lens);
    lens
}

/// The camera's depth of field and motion blur on the render, through its
/// own lens. Returns the node whose `out` is the finished image.
fn tail(b: &mut Builder<'_>, camera: &ItemId, lens: u32) -> u32 {
    let owner = camera.as_str();
    let render = b.scene("render");
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
    blur
}

/// Give the tail's nodes ids from the camera, inside its group too, so two
/// cameras' tails never share one and a recompile keeps them.
fn rename(node: &mut EffectGraphNode, camera: &ItemId) {
    node.node_id = NodeId::new(format!("{camera}:{}", node.node_id));
    node.handle = Some(node.node_id.to_string());
    if let Some(body) = node.group.as_deref_mut() {
        body.nodes
            .iter_mut()
            .for_each(|inner| rename(inner, camera));
    }
}

fn find_type(node: &EffectGraphNode, type_id: &str) -> Option<u32> {
    if node.type_id == type_id {
        return Some(node.id);
    }
    node.group
        .as_deref()?
        .nodes
        .iter()
        .find_map(|inner| find_type(inner, type_id))
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
        let frame = b
            .id(target.as_str(), "pose_frame")
            .or_else(|| b.id(target.as_str(), "transform"));
        if let (Some(rig), Some(frame)) = (b.id(camera.as_str(), "rig"), frame) {
            b.wire(frame, "transform", rig, "target");
        }
    }
}
