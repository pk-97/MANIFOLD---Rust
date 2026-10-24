//! The nodes the compiler emits once per scene, outside any item: the
//! generator input the host clocks, the render node's counts and
//! environment turn, the plain material an object with no material draws
//! with, and the Scene Loop's cell measure. None is on a card.

use manifold_foundation::scene_surface::ControlGroup;

use super::{Exposure, PropertyRow, Reach, row};

const INPUT: Reach = Reach::Scene("input");
const RENDER: Reach = Reach::Scene("render");
const LOOP_CELL: Reach = Reach::Scene("loop_cell");
const HOST: Exposure = Exposure::Automatic("the host writes it each frame");
use ControlGroup::RenderMode;

/// The plain material's params are a family too: every one stays at its
/// default.
pub const DEFAULT_MATERIAL_REASON: &str = "the plain look of an object with no material";

pub static SCENE_LEVEL_ROWS: [PropertyRow; 14] = [
    row("time", INPUT, RenderMode, HOST),
    row("beat", INPUT, RenderMode, HOST),
    row("aspect", INPUT, RenderMode, HOST),
    row("trigger_count", INPUT, RenderMode, HOST),
    row("anim_progress", INPUT, RenderMode, HOST),
    row("output_width", INPUT, RenderMode, HOST),
    row("output_height", INPUT, RenderMode, HOST),
    row("trigger_baseline", INPUT, RenderMode, HOST),
    row(
        "objects",
        RENDER,
        RenderMode,
        Exposure::Automatic("the number of objects drawn"),
    ),
    row(
        "lights",
        RENDER,
        RenderMode,
        Exposure::Automatic("the number of lights"),
    ),
    row(
        "env_rotation",
        RENDER,
        RenderMode,
        Exposure::Automatic("the Environment's HDRI Rotation turns it"),
    ),
    row(
        "loop_cell_boxes",
        LOOP_CELL,
        RenderMode,
        Exposure::Automatic("one box for each object, copy group and swarm the Scene Loop tiles"),
    )
    .param("boxes"),
    row(
        "loop_cell_axis",
        LOOP_CELL,
        RenderMode,
        Exposure::Internal("the corridor runs along Z"),
    )
    .param("axis"),
    row(
        "loop_cell_gap",
        LOOP_CELL,
        RenderMode,
        Exposure::Automatic("the Scene Loop's Spacing"),
    )
    .param("gap"),
];
