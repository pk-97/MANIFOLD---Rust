//! The nodes the compiler emits once per scene, outside any item: the
//! generator input the host clocks, the render node's counts and
//! environment turn, and the plain material an object with no material
//! draws with. None is on a card.

use manifold_foundation::scene_surface::ControlGroup;

use super::{Exposure, PropertyRow, Reach, row};

const INPUT: Reach = Reach::Scene("input");
const RENDER: Reach = Reach::Scene("render");
const HOST: Exposure = Exposure::Automatic("the host writes it each frame");
use ControlGroup::RenderMode;

/// The plain material's params are a family too: every one stays at its
/// default.
pub const DEFAULT_MATERIAL_REASON: &str = "the plain look of an object with no material";

pub static SCENE_LEVEL_ROWS: [PropertyRow; 11] = [
    row("time", INPUT, RenderMode, HOST),
    row("beat", INPUT, RenderMode, HOST),
    row("aspect", INPUT, RenderMode, HOST),
    row("trigger_count", INPUT, RenderMode, HOST),
    row("anim_progress", INPUT, RenderMode, HOST),
    row("output_width", INPUT, RenderMode, HOST),
    row("output_height", INPUT, RenderMode, HOST),
    row("trigger_baseline", INPUT, RenderMode, HOST),
    row("objects", RENDER, RenderMode, Exposure::Automatic("the number of objects drawn")),
    row("lights", RENDER, RenderMode, Exposure::Automatic("the number of lights")),
    row("env_rotation", RENDER, RenderMode, Exposure::Automatic("the Environment's HDRI Rotation turns it")),
];
