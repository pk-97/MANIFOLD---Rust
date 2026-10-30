//! Tree: an Object whose Shape is Tree. `node.tree` grows a trunk, branches
//! and leaves from branching rules on the GPU; the Object draws the bark,
//! and a Leaves Object beside it in the same Group draws the leaves, each
//! with its own Material.
//!
//! Every row but Levels is a performance row, so a tree can grow, spread
//! and droop on the beat. Levels is setup: it sets how many times the
//! branches split.

use manifold_foundation::scene_surface::{CardBand, ControlGroup, Unit};

use super::{Exposure, PropertyRow, Reach, perform, row, setup};
use crate::scene_doc::{ItemFields, Shape};

const S: ControlGroup = ControlGroup::Shape;
const TREE: Reach = Reach::Own("tree");

fn tree(fields: &ItemFields) -> bool {
    matches!(fields, ItemFields::Object(o) if o.shape == Shape::Tree)
}

const fn dial(property: &'static str, param: &'static str, order: u8) -> PropertyRow {
    row(property, TREE, S, perform(CardBand::Scene, order))
        .param(param)
        .when(tree)
}

static ROWS: [PropertyRow; 16] = [
    dial("tree_height", "height", 0)
        .name("Height")
        .unit(Unit::Metres),
    dial("tree_trunk_width", "trunk_width", 1)
        .name("Trunk Width")
        .unit(Unit::Metres),
    dial("tree_branches", "branches", 2).name("Branches"),
    dial("tree_twigs", "twigs", 3).name("Twigs"),
    dial("tree_spread", "spread", 4)
        .name("Spread")
        .unit(Unit::Degrees),
    dial("tree_reach", "reach", 5).name("Reach"),
    dial("tree_start", "start", 6).name("Start"),
    dial("tree_gnarl", "gnarl", 7).name("Gnarl"),
    dial("tree_lift", "lift", 8).name("Lift"),
    dial("tree_twist", "twist", 9)
        .name("Twist")
        .unit(Unit::Degrees),
    dial("tree_leaves", "leaves", 10).name("Leaves"),
    dial("tree_leaf_size", "leaf_size", 11)
        .name("Leaf Size")
        .unit(Unit::Metres),
    dial("tree_leaf_angle", "leaf_angle", 12)
        .name("Leaf Angle")
        .unit(Unit::Degrees),
    dial("tree_seed", "seed", 13).name("Seed"),
    row("tree_levels", TREE, S, setup())
        .param("levels")
        .name("Levels")
        .when(tree),
    row(
        "tree_cards",
        TREE,
        S,
        Exposure::Internal("each leaf is two crossed cards"),
    )
    .param("cards")
    .when(tree),
];

/// Every table a Tree's rows come from.
pub static TREE_TABLES: [&[PropertyRow]; 1] = [&ROWS];
