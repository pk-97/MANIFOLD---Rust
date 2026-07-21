//! Shared constants, types, and builder functions for parameter slider panels.
//!
//! The unified `ParamCardPanel` (effect + generator kinds) uses identical
//! layout constants, driver/envelope config builders, trim/target handle
//! builders, and formatting helpers across both kinds. This module is the
//! single source of truth for them.

use crate::RootAction;
use super::DriverConfigAction;
use super::TrimKind;
use super::param_card::RowMod;
use crate::param_surface::ParamRow;
use super::{AudioShapeParam, GraphParamTarget, PanelAction, ScrubPhase, ScrubValue, ValueRef};
use crate::chrome::{Theme, View};
use crate::color;
use crate::drag::DragController;
use crate::node::*;
use crate::slider::{BitmapSlider, SliderColors, SliderNodeIds};
use crate::tree::UITree;
pub use crate::types::AbletonMappingStatus;
use manifold_foundation::LayerId;

mod builders;
mod state;
mod routing;
mod geometry;
pub(crate) use builders::*;
pub use state::*;
pub(crate) use routing::*;
pub(crate) use geometry::*;

// ── Shared layout constants ─────────────────────────────────────

pub(crate) const ROW_HEIGHT: f32 = 24.0;
pub(crate) const ROW_SPACING: f32 = 6.0;
/// Extra gap below an expanded modulation drawer, before the next param row. The
/// slider hugs its own drawer (ROW_SPACING above); this larger break after it
/// makes the next slider clearly a separate row. Paired in `row_drawer_height`
/// so build + height computation agree.
pub(crate) const DRAWER_BOTTOM_GAP: f32 = color::SPACE_L;
/// Left inset of a modulation drawer from the row's left edge — the same "belongs
/// to its parent" indent grammar as a layer nested inside a group on the timeline
/// (`color::GROUP_CHILD_INDENT_PX`), but a slighter single-level step: it says the
/// drawer is an operation *under* its slider without re-eating the label column the
/// way the old track-width indent did. Affects geometry only (`drawer_x`), not
/// height — height math is unchanged.
pub(crate) const DRAWER_INDENT: f32 = color::SPACE_L;
/// Padding the mod card extends BEYOND its content on the top + left + right, so
/// the slider, value, and arm buttons sit inset from the card edge instead of flush
/// against it (and so the top covers the slider's trim / target handles, which poke
/// a couple px above the track — `build_envelope_target` starts at `track.y - 2`).
/// The bottom needs none: the drawer's own internal `TOP_PAD` already insets the
/// last row. Visual only — does not move content or affect height math.
pub(crate) const MOD_CARD_PAD: f32 = 4.0;
// Card inner inset (§14.5 C). The canonical `SPACE_M`: with the card's 1px frame
// border that puts param-label content at `BORDER_W + SPACE_M` =
// `color::SECTION_CONTENT_INSET`, the one column the border-less chrome panels
// align to. `slider_w` / `label_width` / the header trailing-x all derive from
// this, so they cascade.
pub(crate) const PADDING: f32 = color::SPACE_M;
pub(crate) const GAP: f32 = color::SPACE_S;
// Param rows track the body-text token so the inspector matches layer-control
// chrome on the type ramp (they're the live instrument surface).
pub(crate) const FONT_SIZE: u16 = color::FONT_BODY;

pub(crate) const DE_BUTTON_SIZE: f32 = 20.0;
/// Gap *between* the three T/∿/A arm buttons. Tight, so they read as one group.
pub(crate) const DE_BUTTON_GAP: f32 = color::SPACE_S;
/// Gap between the slider's right edge and the T/∿/A group. Wider than the
/// inter-button gap so the value and the arm buttons don't crowd each other —
/// the slider reads as one cell, the arm group as another.
pub(crate) const MOD_LANE_GAP: f32 = color::SPACE_M;
/// Height of the modulation-config tab strip (only drawn when ≥2 configs active).
pub(crate) const MOD_TAB_STRIP_H: f32 = 18.0;
const MOD_TAB_H: f32 = 16.0;

/// Active tint for the audio-modulation ("A") button + drawer — a clean green,
/// kept distinct from the driver (teal) and envelope (orange) actives. Shares
/// the audio trim-handle green so the whole audio-mod identity reads as one.
pub(crate) const AUDIO_MOD_ACTIVE_C32: crate::node::Color32 = color::AUDIO_TRIM_BAR_C32;
pub(crate) const BEAT_DIV_COUNT: usize = 11;
pub(crate) const WAVEFORM_COUNT: usize = 5;

pub(crate) const ABL_CONFIG_HEIGHT: f32 = 24.0;

/// Full-scale for the audio "Sensitivity" slider: 0..this.
pub(crate) const AUDIO_SENS_MAX: f32 = 4.0;
/// Full-scale for the audio "Attack" slider, in ms: 0..this.
pub(crate) const AUDIO_ATTACK_MAX_MS: f32 = 500.0;
/// Full-scale for the audio "Release" slider, in ms: 0..this.
pub(crate) const AUDIO_RELEASE_MAX_MS: f32 = 2000.0;
/// Leading-label width for the audio shaping sliders.
pub(crate) const AUDIO_SHAPE_LABEL_W: f32 = 52.0;

// `AudioModShape`'s own field defaults (mirrors `manifold_core::audio_mod`'s
// `default_sensitivity()`/`default_attack_ms()`/`default_release_ms()` —
// plain consts here so this crate doesn't need a `manifold-core` type import
// just to know a slider's right-click-reset target, BUG-061).
pub(crate) const AUDIO_SENS_DEFAULT: f32 = 1.0;
pub(crate) const AUDIO_ATTACK_DEFAULT_MS: f32 = 5.0;
pub(crate) const AUDIO_RELEASE_DEFAULT_MS: f32 = 120.0;

// ── PARAM_STEP_ACTIONS D2/D8: the Action/Amount/Wrap rows ──────────────
//
// This crate mirrors core enums rather than depending on `manifold-core`
// directly (the established convention — see `audio_kind_labels`/
// `AudioFeatureKind::ALL` above, and `AudioModSetTriggerMode`'s doc comment).
// `TriggerAction`/`WrapMode` are mirrored the same way.

/// Number of Action choices in the drawer's Action row (`[Continuous, Step,
/// Random]`, D2).
pub(crate) const AUDIO_ACTION_COUNT: usize = 3;
/// Number of Wrap choices in the drawer's Wrap row (`[Wrap, Bounce, Clamp]`,
/// D2), shown only while Action=Step.
pub(crate) const AUDIO_WRAP_COUNT: usize = 3;

/// Length-row musical divisions (beats), for a clip trigger's `one_shot_beats`
/// (P3, D4). Same musical range the deleted Triggers matrix's stepper covered
/// (0.25..16 beats) collapsed to a fixed button row instead of a −/＋ stepper
/// — the drawer's other rows are all fixed button sets, not steppers.
pub(crate) const LENGTH_OPTIONS: [f32; 6] = [0.25, 0.5, 1.0, 2.0, 4.0, 8.0];

// Arming the envelope shows two controls: the orange target handle on the
// parameter's own track (the value it's pulled toward) and a single "Decay"
// slider in a one-row drawer (how fast it falls back).
pub(crate) const ENV_CONFIG_HEIGHT: f32 = 30.0;
pub(crate) const ENV_DECAY_LABEL_W: f32 = 50.0;
/// Decay slider full-scale, in beats (0 → this).
pub(crate) const ENV_DECAY_MAX: f32 = 8.0;
/// Default decay for a freshly-armed envelope — mirrors core's
/// `DEFAULT_ENVELOPE_DECAY_BEATS` so the slider shows a usable value at once.
pub(crate) const DEFAULT_ENV_DECAY: f32 = 1.0;

pub(crate) const TRIM_BAR_W: f32 = 4.0;
pub(crate) const TARGET_BAR_W: f32 = 6.0;
pub(crate) const OVERLAY_INSET: f32 = 1.0;

pub(crate) const BEAT_DIV_LABELS: [&str; BEAT_DIV_COUNT] = [
    "1/32", "1/16", "1/8", "1/4", "1/2", "1", "2", "4", "8", "16", "32",
];

/// Period in beats for each grid button index — mirrors core's
/// `BeatDivision::from_button_index(idx).beats()` (the UI carries only the button
/// index, not the core enum). Quarter ("1/4") = 1 beat; "1" = a whole note = 4
/// beats. Used to prefill the Free type-in with the current sync period.
pub(crate) const BEAT_DIV_BEATS: [f32; BEAT_DIV_COUNT] =
    [0.125, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0, 128.0];

/// Number of feature kinds / bands exposed in the drawer.
pub(crate) const AUDIO_KIND_COUNT: usize = crate::types::AudioFeatureKind::ALL.len();
pub(crate) const AUDIO_BAND_COUNT: usize = 4;

#[cfg(test)]
mod length_row_tests {
    use super::*;

    // P3 D4/D5: the drawer's new Length row — `format_beats` (moved from the
    // deleted Triggers matrix), `length_labels`, and `length_option_index`.
    // These are the pure functions `build_audio_mod_drawer`'s Length row
    // (`length_beats: Option<f32>`) is built from; a UITree-level test of the
    // drawer itself would need to replicate `param_card.rs`'s fixture
    // scaffolding, so correctness of the row's actual content is proven here
    // at the value level, matching this crate's usual split (pure logic
    // tested directly, layout proven by the headless PNG demo).

    #[test]
    fn format_beats_matches_musical_divisions() {
        assert_eq!(format_beats(0.25), "1/4");
        assert_eq!(format_beats(0.5), "1/2");
        assert_eq!(format_beats(1.0), "1b");
        assert_eq!(format_beats(2.0), "2b");
        assert_eq!(format_beats(4.0), "4b");
        assert_eq!(format_beats(8.0), "8b");
    }

    #[test]
    fn length_labels_are_format_beats_of_length_options() {
        let labels = length_labels();
        for (label, beats) in labels.iter().zip(LENGTH_OPTIONS.iter()) {
            assert_eq!(label, &format_beats(*beats));
        }
    }

    #[test]
    fn length_option_index_snaps_to_nearest() {
        // Exact hits.
        assert_eq!(length_option_index(0.25), 0);
        assert_eq!(length_option_index(1.0), 2);
        assert_eq!(length_option_index(8.0), 5);
        // A legacy-migrated value that doesn't land exactly on an option
        // (BUG-079's sensitivity→Amount U5 mapping is the same "not exact,
        // snap" shape) snaps to the closer neighbor.
        assert_eq!(length_option_index(0.9), 2, "0.9 nearer to 1.0 than 0.5");
        assert_eq!(length_option_index(0.6), 1, "0.6 nearer to 0.5 than 1.0");
        assert_eq!(length_option_index(0.4), 1, "0.4 nearer to 0.5 than 0.25");
        assert_eq!(length_option_index(100.0), 5, "clamps to the largest option");
        assert_eq!(length_option_index(0.0), 0, "clamps to the smallest option");
    }
}
