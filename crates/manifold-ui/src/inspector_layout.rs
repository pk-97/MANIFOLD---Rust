// Shared inspector panel dimensions used by MasterInspector, LayerInspector,
// ClipInspector, and GenParamPresenter.
// All spatial values scale with `widget_layout::SCALE`.
//
// Mechanical translation of Assets/Scripts/UI/Timeline/Core/InspectorLayout.cs.

use crate::color;
use crate::widget_layout::SCALE as S;

// ── Section headers ──────────────────────────────────────────────
pub const SECTION_HEADER_HEIGHT: f32 = 24.0 * S; // §14.3: 22 → 24, one row rhythm
pub const SECTION_FONT_SIZE: i32 = (13.0 * S) as i32;

// ── Common rows ──────────────────────────────────────────────────
pub const DIVIDER_HEIGHT: f32 = 1.0;
pub const NAME_ROW_HEIGHT: f32 = 20.0 * S;
pub const SMALL_ROW_HEIGHT: f32 = 18.0 * S;
pub const ACTION_BUTTON_HEIGHT: f32 = 28.0 * S;

// ── Content padding (MasterInspector + LayerInspector) ───────────
// All spatial values reference the global `SPACE_*` scale (§14.2 rule 8). The
// horizontal inset is the canonical `SPACE_M`; vertical pad/spacing are `SPACE_S`.
pub const CONTENT_PADDING_H: i32 = (color::SPACE_M * S) as i32;
pub const CONTENT_PADDING_V: i32 = (color::SPACE_S * S) as i32; // §14.3: 6 → 4
pub const CONTENT_SPACING: f32 = color::SPACE_S * S;

// ── Clip inspector (differs slightly) ────────────────────────────
pub const CLIP_HEADER_HEIGHT: f32 = 24.0 * S;
pub const CLIP_SPACING: f32 = color::SPACE_S * S; // §14.3: 6 → 4
// `CLIP_PADDING_H` (10) stays put — collapsing it to the canonical 8 inset moves
// horizontal alignment, so it lands in the structural inset-unification step (§14.5 C).
pub const CLIP_PADDING_H: i32 = (10.0 * S) as i32;
pub const CLIP_PADDING_V: i32 = (color::SPACE_M * S) as i32;

// ── Effect containers ────────────────────────────────────────────
// Inter-card gap is owned jointly with `param_card::CARD_BOTTOM_MARGIN`; the two
// move together in the gap-ownership step (§14.5 E), so this stays 3 for now.
pub const EFFECT_CONTAINER_SPACING: f32 = 3.0 * S;
