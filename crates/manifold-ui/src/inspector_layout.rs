// Shared inspector panel dimensions used by MasterInspector, LayerInspector,
// ClipInspector, and GenParamPresenter.
// All spatial values scale with `widget_layout::SCALE`.
//
// Mechanical translation of Assets/Scripts/UI/Timeline/Core/InspectorLayout.cs.

use crate::widget_layout::SCALE as S;

// ── Section headers ──────────────────────────────────────────────
pub const SECTION_HEADER_HEIGHT: f32 = 22.0 * S;
pub const SECTION_FONT_SIZE: i32 = (13.0 * S) as i32;

// ── Common rows ──────────────────────────────────────────────────
pub const DIVIDER_HEIGHT: f32 = 1.0;
pub const NAME_ROW_HEIGHT: f32 = 20.0 * S;
pub const SMALL_ROW_HEIGHT: f32 = 18.0 * S;
pub const ACTION_BUTTON_HEIGHT: f32 = 28.0 * S;

// ── Content padding (MasterInspector + LayerInspector) ───────────
pub const CONTENT_PADDING_H: i32 = (8.0 * S) as i32;
pub const CONTENT_PADDING_V: i32 = (6.0 * S) as i32;
pub const CONTENT_SPACING: f32 = 4.0 * S;

// ── Clip inspector (differs slightly) ────────────────────────────
pub const CLIP_HEADER_HEIGHT: f32 = 24.0 * S;
pub const CLIP_SPACING: f32 = 6.0 * S;
pub const CLIP_PADDING_H: i32 = (10.0 * S) as i32;
pub const CLIP_PADDING_V: i32 = (8.0 * S) as i32;

// ── Effect containers ────────────────────────────────────────────
pub const EFFECT_CONTAINER_SPACING: f32 = 3.0 * S;
