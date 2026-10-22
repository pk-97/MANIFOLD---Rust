//! Group: its transform (the frame everything inside it sits in) and its
//! Visible, which multiplies down the tree through each nested group.

use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{ControlGroup, Unit};

use super::{Exposure, PropertyRow, Reach, row, setup};

const T: Reach = Reach::Own("transform");
const VISIBLE: Reach = Reach::Own("visible");
const CHAIN: Reach = Reach::Own("visible_chain");
use ControlGroup::{Look, Transform};

pub static GROUP_ROWS: [PropertyRow; 14] = [
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
];
