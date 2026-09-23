//! `node.terminal_stream` — a bounded, beat-driven CPU terminal source.
//!
//! With a reaction image wired, local image measurements drive independent
//! edits to long terminal lines. Completed lines hold still; unwired reaction
//! supplies a neutral stationary terminal. Layout selects a single cell or tmux
//! panes. Fixed storage and a fenced analysis ring keep the frame path bounded.

use manifold_core::Beats;
use std::borrow::Cow;

use super::terminal_analysis::TerminalAnalysis;
use super::terminal_detail::{DetailFrame, TerminalDetail};
use super::terminal_reaction::ReactiveTerminal;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const MAX_CELLS: usize = 640 * 135;
const MAX_COLS: u32 = 640;
const MAX_ROWS: u32 = 135;
const DEFAULT_TEXT_SIZE: f32 = 18.0;

/// The output buffer always has this capacity. `run` writes only
/// `columns * rows` cells, so downstream consumers can use the scalar outputs
/// as the active dimensions while the physical allocation stays stable.
pub const TERMINAL_STREAM_CAPACITY: u32 = MAX_CELLS as u32;

/// Result of the canvas-to-grid calculation, kept separate from the runtime so
/// it can be tested without a GPU backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GridDimensions {
    columns: u32,
    rows: u32,
}

fn grid_dimensions(width: u32, height: u32, text_size: f32) -> GridDimensions {
    let size = if text_size.is_finite() {
        text_size.clamp(8.0, 48.0)
    } else {
        DEFAULT_TEXT_SIZE
    };
    let columns = if width == 0 || height == 0 {
        1
    } else {
        ((f64::from(width) / f64::from(height) * 1080.0 / (f64::from(size) * 0.6)).floor() as u32)
            .clamp(1, MAX_COLS)
    };
    let rows = ((1080.0 / size).floor() as u32).clamp(1, MAX_ROWS);
    GridDimensions { columns, rows }
}

crate::primitive! {
    name: TerminalStream,
    type_id: "node.terminal_stream",
    purpose: "Emit long source-driven terminal lines in a single terminal or bordered tmux layouts. Image structure changes statement content; shell, code, log and inspection passages have distinct edit rhythms, with at most three edits active. Detail Reactivity selectively cycles data characters on fine image changes, then settles. Zero detail bypasses these edits; still input produces still text; unwired reaction holds a neutral terminal. Activity controls edit speed and zero freezes cells; Text Size controls the 1080p-reference grid.",
    inputs: {
        canvas: Texture2D required,
        reaction: Texture2D optional,
        text_size: ScalarF32 optional,
        activity: ScalarF32 optional,
        layout: ScalarF32 optional,
        detail_reactivity: ScalarF32 optional,
    },
    outputs: {
        cells: Channels[VALUE: U32],
        columns: ScalarF32,
        rows: ScalarF32,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("detail_reactivity"),
            label: "Detail Reactivity",
            ty: ParamType::Float,
            default: ParamValue::Float(0.55),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("text_size"),
            label: "Text Size",
            ty: ParamType::Float,
            default: ParamValue::Float(DEFAULT_TEXT_SIZE),
            range: Some((8.0, 48.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("activity"),
            label: "Activity",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("layout"),
            label: "Layout",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, 3.0)),
            enum_values: &["Single", "Vertical Split", "Horizontal Split", "Four Panes"],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "canvas supplies dimensions; optional reaction supplies the image. Layout selects Single (0), Vertical Split (1), Horizontal Split (2), or Four Panes (3). Single alternates eight-row vocabulary blocks; panes use shell, code, logs and inspection roles. Continuous statements span each row so downstream code dithering retains image coverage. Bright and dark structure changes the statement data. Completed lines hold until their source profile changes; only changed spans are typed, prioritized by image change and bounded to three concurrent edits. No independent clock animation. A single analysis dispatch produces 64×36 line measurements and 256×144 fine measurements, read only after its GPU fence completes, normally one frame later. Detail Reactivity (0..1, default 0.55) controls character density and change sensitivity. Digits, hex payloads and numeric punctuation cycle for at most 0.45 active beats after a local change, with a noise deadband; words, whitespace and pane borders remain intact. Activity zero freezes both layers. Reuse bounded storage and hold the last completed analysis if all readback slots are busy. Cells stay printable ASCII plus cursor 127, with fixed 86400-u32 capacity; columns/rows describe the active grid. Palette, glyph rendering and erosion remain downstream graph operations. This CPU readback/upload is an IoBridge fusion boundary.",
    examples: [],
    picker: { label: "Terminal Stream", category: Atom },
    summary: "Image-reactive shell, code and logs with source-driven typing and optional tmux panes.",
    category: Generate,
    role: Source,
    aliases: ["terminal", "live terminal", "shell stream", "console source"],
    boundary_reason: IoBridge,
    extra_fields: {
        reactive: ReactiveTerminal = ReactiveTerminal::new(),
        analysis: TerminalAnalysis = TerminalAnalysis::new(),
        detail: TerminalDetail = TerminalDetail::new(),
        reaction_connected: bool = false,
        reaction_ready: bool = false,
        reaction_beat: Option<Beats> = None,
    },
}

impl Primitive for TerminalStream {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "cells").then_some(TERMINAL_STREAM_CAPACITY)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(canvas) = ctx.inputs.texture_2d("canvas") else {
            return;
        };
        let text_size = ctx.scalar_or_param("text_size", DEFAULT_TEXT_SIZE);
        let activity = ctx.scalar_or_param("activity", 1.0);
        let detail_reactivity = ctx.scalar_or_param("detail_reactivity", 0.55);
        let layout_default = match ctx.params.get("layout") {
            Some(ParamValue::Enum(value)) => *value as f32,
            _ => 0.0,
        };
        let layout = ctx
            .scalar_or_param("layout", layout_default)
            .round()
            .clamp(0.0, 3.0) as u8;
        let dimensions = grid_dimensions(canvas.width, canvas.height, text_size);
        let reaction = ctx.inputs.texture_2d("reaction");
        let beat = ctx.time.beats;
        if reaction.is_some() != self.reaction_connected
            || self.reaction_beat.is_some_and(|previous| beat < previous)
        {
            self.reactive.reset();
            self.detail.reset();
            self.analysis.reset();
            self.reaction_ready = false;
        }
        self.reaction_connected = reaction.is_some();
        self.reaction_beat = Some(beat);
        if let Some(image) = reaction {
            let gpu = ctx.gpu_encoder();
            self.analysis.sample(gpu, image);
            // Bootstrap finished text from the first fenced source snapshot.
            // Activity zero must still hold the already visible cells.
            if !self.reaction_ready && self.analysis.has_samples() && activity > 0.0 {
                self.reactive.reset();
                self.detail.reset();
                self.reaction_ready = true;
            }
            self.reactive.update(
                dimensions.columns,
                dimensions.rows,
                layout,
                beat,
                activity,
                self.analysis.latest_samples(),
            );
        } else {
            self.reactive.update(
                dimensions.columns,
                dimensions.rows,
                layout,
                beat,
                activity,
                &super::terminal_reaction::EMPTY_SAMPLES,
            );
        }
        let cells = self.detail.update(
            self.reactive.cells(),
            self.analysis.latest_detail_samples(),
            DetailFrame {
                columns: dimensions.columns as usize,
                rows: dimensions.rows as usize,
                layout,
                beat,
                activity,
                amount: detail_reactivity,
            },
        );

        ctx.outputs
            .set_scalar("columns", ParamValue::Float(dimensions.columns as f32));
        ctx.outputs
            .set_scalar("rows", ParamValue::Float(dimensions.rows as f32));

        let Some(dst) = ctx.outputs.array("cells") else {
            return;
        };
        let capacity = (dst.size / std::mem::size_of::<u32>() as u64) as usize;
        let active = (dimensions.columns as usize)
            .saturating_mul(dimensions.rows as usize)
            .min(capacity)
            .min(MAX_CELLS);
        if active != 0 {
            unsafe { dst.write(0, bytemuck::cast_slice(&cells[..active])) };
        }
    }

    fn clear_state(&mut self) {
        self.reaction_ready = false;
        self.reactive.reset();
        self.detail.reset();
        self.analysis.reset();
        self.reaction_beat = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn dimensions_follow_reference_height_and_bounds() {
        assert_eq!(
            grid_dimensions(1920, 1080, 18.0),
            GridDimensions {
                columns: 177,
                rows: 60
            }
        );
        assert_eq!(grid_dimensions(1, 1, 8.0).columns, 225);
        assert_eq!(grid_dimensions(u32::MAX, 1080, 8.0).columns, MAX_COLS);
        assert_eq!(grid_dimensions(1920, 1080, 0.0).rows, 135);
        assert_eq!(grid_dimensions(1920, 1080, 1000.0).rows, 22);
    }

    #[test]
    fn declares_canvas_and_typed_cells_contract() {
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(TerminalStream::TYPE_ID, "node.terminal_stream");
        assert_eq!(TerminalStream::INPUTS.len(), 6);
        assert_eq!(TerminalStream::INPUTS[0].name, "canvas");
        assert!(TerminalStream::INPUTS[0].required);
        assert_eq!(TerminalStream::INPUTS[0].ty, PortType::Texture2D);
        assert_eq!(TerminalStream::INPUTS[1].name, "reaction");
        assert!(!TerminalStream::INPUTS[1].required);
        assert_eq!(TerminalStream::OUTPUTS.len(), 3);
        assert_eq!(TerminalStream::OUTPUTS[0].name, "cells");
        assert_eq!(
            TerminalStream::OUTPUTS[1].ty,
            PortType::Scalar(ScalarType::F32)
        );
        assert_eq!(
            TerminalStream::OUTPUTS[2].ty,
            PortType::Scalar(ScalarType::F32)
        );
        assert_eq!(TERMINAL_STREAM_CAPACITY, 86400);
        assert_eq!(TerminalStream::PARAMS[3].default, ParamValue::Enum(0));
        assert_eq!(TerminalStream::PARAMS[3].enum_values.len(), 4);
    }

    #[test]
    fn output_capacity_is_fixed_and_unknown_ports_are_rejected() {
        let stream = TerminalStream::new();
        let params = crate::node_graph::effect_node::ParamValues::default();
        assert_eq!(
            stream.array_output_capacity("cells", &params, &[]),
            Some(86400)
        );
        assert_eq!(stream.array_output_capacity("other", &params, &[]), None);
    }
}
