//! Group: its transform (the frame everything inside it sits in) and its
//! Visible, which multiplies down the tree through each nested group. An
//! import Group (one whose Source is a file) also plays the file's animation:
//! its Animation rows drive the clock of every animated part below it, and
//! its clip field picks the clip.

use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{ControlGroup, Unit};

use super::{Exposure, PropertyRow, Reach, row, setup};
use crate::scene_doc::ItemFields;

const T: Reach = Reach::Own("transform");
const VISIBLE: Reach = Reach::Own("visible");
const CHAIN: Reach = Reach::Own("visible_chain");
use ControlGroup::{Animation, Look, Transform};

fn import(fields: &ItemFields) -> bool {
    matches!(fields, ItemFields::Group(g) if g.source.is_some())
}

pub static GROUP_ROWS: [PropertyRow; 18] = [
    row("pos_x", T, Transform, setup()).unit(Unit::Metres).limit(HardLimit::Unlimited),
    row("pos_y", T, Transform, setup()).unit(Unit::Metres).limit(HardLimit::Unlimited),
    row("pos_z", T, Transform, setup()).unit(Unit::Metres).limit(HardLimit::Unlimited),
    row("rot_x", T, Transform, setup()).limit(HardLimit::Unlimited),
    row("rot_y", T, Transform, setup()).limit(HardLimit::Unlimited),
    row("rot_z", T, Transform, setup()).limit(HardLimit::Unlimited),
    row("scale_x", T, Transform, setup()),
    row("scale_y", T, Transform, setup()),
    row("scale_z", T, Transform, setup()),
    row("billboard", T, Transform, Exposure::Internal("only an Object turns to face the camera")),
    row("visible", VISIBLE, Look, setup()).param("value").default(1.0).name("Visible").limit(HardLimit::Between(0.0, 1.0)),
    row("chain_a", CHAIN, Look, Exposure::Automatic("this group's Visible")).param("a"),
    row("chain_b", CHAIN, Look, Exposure::Automatic("the enclosing group's visibility")).param("b"),
    row("chain_op", CHAIN, Look, Exposure::Automatic("Multiply, so a hidden outer group hides everything inside it"))
        .param("op"),
    // The old importer's Animation section: Rate, Loop Mode and Retrigger
    // fan out to every clock in the file; the clip is the Group's field.
    row("anim_rate", Reach::Clocks, Animation, setup()).param("rate").when(import),
    row("anim_loop_mode", Reach::Clocks, Animation, setup()).param("loop_mode").when(import),
    row("anim_retrigger", Reach::Clocks, Animation, setup()).param("trigger_count").when(import),
    row("anim_clip", Reach::Clocks, Animation, Exposure::Automatic("the Group's clip field")).param("clip_index").when(import),
];
