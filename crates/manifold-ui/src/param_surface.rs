//! The queryable param-surface model — `docs/WIDGET_TREE_DESIGN.md` D1–D3.
//!
//! One `ParamSurface` describes one manifest-backed card (effect, generator,
//! editor card, scene section): identity + descriptor + state per row, built
//! app-side by the ONE projection (`ui_bridge::state_sync::param_surface`)
//! and consumed by the card renderer, the gesture router, the tests, and the
//! dump. There is no second source: every row fact enters through the
//! projection's single manifest walk.
//!
//! # Adding a row affordance (the §5b recipe — five steps, never five files)
//!
//! 1. Add the `RowRole` variant (P2; this module).
//! 2. Add the fact as ONE field on [`ParamRow`] (or its sub-structs) and its
//!    one line in the projection.
//! 3. Add the render arm in the row builder (`param_card.rs`).
//! 4. Add the `row_action` arm (P2).
//! 5. Add the dispatch test (`ui_bridge/inspector.rs` Harness pattern).
//!
//! Anything that can't be expressed this way is an escalation, by definition
//! of the layer. Building row/slider/drawer machinery anywhere else is
//! forbidden (WIDGET_TREE_DESIGN §5b, Peter's standing rule, INV-8).

use crate::panels::param_card::{ParamCardKind, ParamCardStringInfo, RelightCardConfig, RowMod};
use crate::panels::param_slider_shared::{AbletonMappingDisplay, AudioCardState};
use crate::panels::GraphParamTarget;
use manifold_foundation::{EffectId, LayerId, ParamId};

/// Descriptor half of a row — sourced verbatim from the manifest's
/// `ParamSpecDef` fields by the projection. Never from registry re-reads,
/// never from hand tables (INV-1).
#[derive(Debug, Clone, PartialEq)]
pub struct RowSpec {
    pub name: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub whole_numbers: bool,
    /// Display-only: storage stays radians; the value cell shows degrees.
    pub is_angle: bool,
    /// Renders as a boolean ON/OFF button row instead of a slider.
    pub is_toggle: bool,
    /// Momentary "fire once" button row.
    pub is_trigger: bool,
    /// Trigger-gate toggle row (reaches the audio "A" drawer). Always paired
    /// with `is_toggle: true, is_trigger: false`.
    pub is_trigger_gate: bool,
    /// Named value labels for discrete params; shown instead of the number.
    pub value_labels: Option<Vec<String>>,
    /// Card-bundling section name; contiguous `Some(name)` runs share one
    /// collapsible header. Straight off the manifest spec.
    pub section: Option<String>,
}

/// Value state at projection time. Per-frame effective values keep riding
/// the `sync_values` slot stream (positional over the manifest order,
/// length-asserted — INV-6); these fields are the structural-sync snapshot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RowValue {
    /// User-intended base (pre-modulation).
    pub base: f32,
    /// Post-modulation — what the slider shows.
    pub effective: f32,
    /// Exposed as a slider on the card (hidden slots keep slot-index
    /// semantics for drivers/mappings).
    pub exposed: bool,
    /// Wire-fed (read-only "driven" presentation). Filled where the caller
    /// knows (editor snapshot); `false` elsewhere.
    pub driven: bool,
}

/// Mapping facts for one row.
#[derive(Debug, Clone, PartialEq)]
pub struct RowMapping {
    /// OSC address; label click copies it.
    pub osc_address: Option<String>,
    /// Ableton mapping sub-section, when mapped.
    pub ableton_display: Option<AbletonMappingDisplay>,
    /// Ableton trim range `(min, max)` — trim handles on the track.
    pub ableton_range: Option<(f32, f32)>,
    /// Row carries a per-instance editable reshape (mapping-drawer chevron
    /// in `CardContext::Author`).
    pub mappable: bool,
}

/// One card row: identity + descriptor + state. THE unit of the layer —
/// `id` is the WidgetId salt (P2), the wire identity
/// (`PanelAction`s carry it), and the test address.
#[derive(Debug, Clone)]
pub struct ParamRow {
    pub id: ParamId,
    pub spec: RowSpec,
    pub value: RowValue,
    /// Driver/envelope/automation facts (audio rides
    /// [`ParamSurface::audio`]`.rows`, row-indexed — same order).
    pub modulation: RowMod,
    pub mapping: RowMapping,
}

/// The complete queryable description of one manifest-backed param surface.
/// Replaces the former parallel-vecs card config (P1b): rows are id-keyed
/// structs, aggregates are derived, positional index maps do not exist.
#[derive(Debug, Clone)]
pub struct ParamSurface {
    pub kind: ParamCardKind,
    /// Display name — the effect name or the generator type name.
    pub title: String,
    pub collapsed: bool,
    pub enabled: bool,

    // ── Effect-only identity + flags (defaults for generators) ──
    pub effect_index: usize,
    pub effect_id: EffectId,
    pub supports_envelopes: bool,
    /// Per-card graph override exists (pink "MOD" badge + header tint).
    pub has_graph_mod: bool,

    // ── Generator-only identity ──
    pub layer_id: Option<LayerId>,

    /// The rows, manifest order == render order.
    pub rows: Vec<ParamRow>,
    /// Generator string params (clickable text-field rows). Empty for effects.
    pub string_params: Vec<ParamCardStringInfo>,
    /// Audio-modulation state: `audio.rows[i]` is row `i`'s audio facts
    /// (same order as `rows`); the card-level send list rides alongside.
    pub audio: AudioCardState,
    /// "3D Shading" toggle + knobs.
    pub relight: RelightCardConfig,
}

impl ParamSurface {
    /// The card's wire identity — derived, never stored twice.
    pub fn target(&self) -> GraphParamTarget {
        match self.kind {
            ParamCardKind::Effect => GraphParamTarget::Effect(self.effect_index),
            ParamCardKind::Generator => GraphParamTarget::Generator,
        }
    }

    /// DRV badge: any row has an active driver. Derived (the stored
    /// aggregate mirrors died with the old parallel-vecs card config).
    pub fn has_drv(&self) -> bool {
        self.rows.iter().any(|r| r.modulation.driver_active)
    }

    /// ENV badge: any row has an active envelope.
    pub fn has_env(&self) -> bool {
        self.rows.iter().any(|r| r.modulation.envelope_active)
    }

    /// ABL badge: any row has an Ableton mapping.
    pub fn has_abl(&self) -> bool {
        self.rows.iter().any(|r| r.mapping.ableton_display.is_some())
    }
}
