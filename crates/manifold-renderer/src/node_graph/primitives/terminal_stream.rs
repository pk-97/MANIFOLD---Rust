//! `node.terminal_stream` — a bounded, beat-driven CPU terminal source.
//!
//! With a reaction image wired, local image measurements drive independent
//! typing, erasing and rewriting regions throughout the screen. Without it,
//! the original scrolling shell remains available. Fixed CPU storage and a
//! fenced analysis ring keep the frame path bounded and nonblocking.

use manifold_core::Beats;
use std::borrow::Cow;

use super::terminal_analysis::TerminalAnalysis;
use super::terminal_reaction::ReactiveTerminal;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const MAX_CELLS: usize = 640 * 135;
const MAX_COLS: u32 = 640;
const MAX_ROWS: u32 = 135;
const PANE_WIDTH: u32 = 60;
const MAX_PANES: usize = MAX_COLS.div_ceil(PANE_WIDTH) as usize;
const DEFAULT_TEXT_SIZE: f32 = 18.0;
const BASE_CHARS_PER_BEAT: f64 = 24.0;
const MAX_CHARS_PER_FRAME: u64 = 4096;
const INITIAL_TYPING_LENGTH: u16 = 2;
const TICK_UNITS: u64 = 1_000_000;
const BLINK_PERIOD_UNITS: u64 = 12 * TICK_UNITS;
const TYPED_LINE_PAUSE_TICKS: u16 = 8;
const BATCH_LINE_PAUSE_TICKS: u16 = 4;
const PANE_HISTORY_OFFSET: u64 = 37;

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

/// Fixed-size virtual terminal state. The content thread mutates this state;
/// no string or vector is allocated while a frame is being rendered.
pub(super) struct TerminalState {
    cells: Box<[u32]>,
    line_scratch: [u8; PANE_WIDTH as usize],
    completed_lines: [u64; MAX_PANES],
    typing_lengths: [u16; MAX_PANES],
    pause_ticks: [u16; MAX_PANES],
    character_phases: [u64; MAX_PANES],
    blink_phases: [u64; MAX_PANES],
    last_beat: Option<Beats>,
    fractional_tick: f64,
    columns: u32,
    rows: u32,
    initialized: bool,
}

impl TerminalState {
    fn new() -> Self {
        Self {
            cells: vec![b' ' as u32; MAX_CELLS].into_boxed_slice(),
            line_scratch: [b' '; PANE_WIDTH as usize],
            completed_lines: [0; MAX_PANES],
            typing_lengths: [0; MAX_PANES],
            pause_ticks: [0; MAX_PANES],
            character_phases: [0; MAX_PANES],
            blink_phases: [0; MAX_PANES],
            last_beat: None,
            fractional_tick: 0.0,
            columns: 0,
            rows: 0,
            initialized: false,
        }
    }

    fn reset(&mut self, dimensions: GridDimensions) {
        self.cells.fill(b' ' as u32);
        self.line_scratch.fill(b' ');
        let history_start = (dimensions.rows.saturating_sub(1)) as u64 + 1000;
        for (pane, line) in self.completed_lines.iter_mut().enumerate() {
            *line = history_start + pane as u64 * PANE_HISTORY_OFFSET;
        }
        // Keep the prompt / first two bytes visible on every pane on a fresh
        // frame, so the source starts with completed history plus a live line.
        self.typing_lengths.fill(INITIAL_TYPING_LENGTH);
        self.pause_ticks.fill(0);
        self.character_phases.fill(0);
        self.blink_phases.fill(0);
        self.last_beat = None;
        self.fractional_tick = 0.0;
        self.columns = dimensions.columns;
        self.rows = dimensions.rows;
        self.initialized = true;
    }

    fn ensure_dimensions(&mut self, dimensions: GridDimensions) {
        if !self.initialized {
            self.reset(dimensions);
            return;
        }
        self.columns = dimensions.columns;
        self.rows = dimensions.rows;
        let pane_count = pane_count(dimensions.columns);
        for pane in 0..pane_count {
            self.typing_lengths[pane] =
                self.typing_lengths[pane].clamp(INITIAL_TYPING_LENGTH, PANE_WIDTH as u16);
        }
    }

    /// Advance by beat delta. The phase is accumulated rather than derived
    /// from absolute beat position, so changing activity changes the rate
    /// without jumping the current typing position.
    fn advance(&mut self, beat: Beats, activity: f32) {
        let Some(previous_beat) = self.last_beat else {
            self.last_beat = Some(beat);
            return;
        };
        if beat < previous_beat {
            let dimensions = GridDimensions {
                columns: self.columns,
                rows: self.rows,
            };
            self.reset(dimensions);
            self.last_beat = Some(beat);
            return;
        }
        self.last_beat = Some(beat);
        let delta = (beat - previous_beat).0;
        if delta <= 0.0 || !delta.is_finite() || activity <= 0.0 || !activity.is_finite() {
            return;
        }

        let pane_count = pane_count(self.columns);
        let exact_units =
            delta * f64::from(activity.clamp(0.0, 4.0)) * BASE_CHARS_PER_BEAT * TICK_UNITS as f64
                + self.fractional_tick;
        let base_units = exact_units.round() as u64;
        self.fractional_tick = exact_units - base_units as f64;
        for pane in 0..pane_count {
            // The base tick accumulator is independent of the current line.
            // Consume it one tick at a time so a line boundary produces the
            // same pause/burst behaviour regardless of frame subdivision.
            self.character_phases[pane] = self.character_phases[pane].saturating_add(base_units);
            self.blink_phases[pane] =
                self.blink_phases[pane].wrapping_add(base_units) % BLINK_PERIOD_UNITS;
            let mut steps = 0;
            while steps < MAX_CHARS_PER_FRAME {
                let cost = self.current_tick_cost(pane);
                if self.character_phases[pane] < cost {
                    break;
                }
                self.character_phases[pane] -= cost;
                self.type_one_tick(pane);
                steps += 1;
            }
        }
    }

    fn current_tick_cost(&self, pane: usize) -> u64 {
        let line_index = line_template_index(self.completed_lines[pane], pane);
        if is_batch_line(line_index) {
            TICK_UNITS / 2
        } else {
            TICK_UNITS
        }
    }

    fn type_one_tick(&mut self, pane: usize) {
        if self.pause_ticks[pane] != 0 {
            self.pause_ticks[pane] -= 1;
            if self.pause_ticks[pane] == 0 {
                self.completed_lines[pane] = self.completed_lines[pane].wrapping_add(1);
                self.typing_lengths[pane] = INITIAL_TYPING_LENGTH;
            }
            return;
        }
        let line_number = self.completed_lines[pane];
        let length = self.make_line(line_number, pane);
        if is_batch_line(line_template_index(line_number, pane)) {
            // Log/status lines arrive as a compact batch instead of looking
            // like a person typed every character.
            if self.typing_lengths[pane] < length as u16 {
                self.typing_lengths[pane] = length as u16;
                return;
            }
        } else if self.typing_lengths[pane] < length as u16 {
            self.typing_lengths[pane] += 1;
            return;
        }

        if self.pause_ticks[pane] == 0 {
            self.pause_ticks[pane] = if is_batch_line(line_template_index(line_number, pane)) {
                BATCH_LINE_PAUSE_TICKS
            } else {
                TYPED_LINE_PAUSE_TICKS
            };
        }
    }

    fn render(&mut self) {
        let columns = self.columns as usize;
        let rows = self.rows as usize;
        let active = columns.saturating_mul(rows).min(MAX_CELLS);
        self.cells[..active].fill(b' ' as u32);
        if columns == 0 || rows == 0 {
            return;
        }

        let panes = pane_count(self.columns);
        for pane in 0..panes {
            let (pane_start, pane_end) = pane_bounds(columns, pane, panes);
            let pane_columns = pane_end - pane_start;
            for row in 0..rows {
                let line_number = if row + 1 == rows {
                    self.completed_lines[pane]
                } else {
                    self.completed_lines[pane].saturating_sub((rows - 1 - row) as u64)
                };
                let line_length = self.make_line(line_number, pane);
                let is_typing_line = row + 1 == rows;
                let visible_length = if is_typing_line {
                    line_length.min(self.typing_lengths[pane] as usize)
                } else {
                    line_length
                };
                for local_x in 0..pane_columns {
                    let index = row * columns + pane_start + local_x;
                    let cursor = is_typing_line
                        && self.blink_phases[pane] < BLINK_PERIOD_UNITS / 2
                        && local_x == visible_length
                        && visible_length < pane_columns;
                    self.cells[index] = if cursor {
                        127
                    } else if local_x < visible_length {
                        u32::from(self.line_scratch[local_x])
                    } else {
                        b' ' as u32
                    };
                }
            }
        }
    }

    /// Fill one bounded line with a coherent shell command or log message.
    /// Numeric and hexadecimal fields change with the virtual line number;
    /// they are formatted directly into the fixed scratch array.
    fn make_line(&mut self, line_number: u64, pane: usize) -> usize {
        self.line_scratch.fill(b' ');
        const TEMPLATES: [&[u8]; 12] = [
            b"$ tail -f /var/log/render.log",
            b"$ ssh visual@render-02 'uptime'",
            b"[  OK  ] stream sync beat=",
            b"/srv/media/live/clip_",
            b"frame=",
            b"tcp ESTAB 10.0.0.2:22 -> 10.0.0.",
            b"0x",
            b"buffer[",
            b"$ for clip in /show/*.mov; do",
            b"  ffprobe -v error \"$clip\"",
            b"  printf '%s\\n' \"$clip\"",
            b"done",
        ];
        let template_index = line_template_index(line_number, pane);
        let mut length = copy_bytes(&mut self.line_scratch, 0, TEMPLATES[template_index]);
        match template_index {
            2 | 4 => {
                length = append_decimal(&mut self.line_scratch, length, line_number);
                length = append_bytes(&mut self.line_scratch, length, b"  pts=0x");
                length = append_hex(
                    &mut self.line_scratch,
                    length,
                    line_number.wrapping_mul(104729) ^ ((pane as u64) << 20),
                );
                length = append_bytes(&mut self.line_scratch, length, b"  status=ready");
            }
            3 => {
                length = append_decimal(&mut self.line_scratch, length, line_number);
                length = append_bytes(&mut self.line_scratch, length, b".mov");
            }
            5 => {
                length = append_decimal(&mut self.line_scratch, length, 10 + pane as u64);
                length = append_bytes(&mut self.line_scratch, length, b":");
                length =
                    append_decimal(&mut self.line_scratch, length, 40000 + line_number % 10000);
            }
            6 => {
                length = append_hex(
                    &mut self.line_scratch,
                    length,
                    line_number.wrapping_mul(65537),
                );
                length = append_bytes(
                    &mut self.line_scratch,
                    length,
                    b"  48 8b 05 31 c0 0f 1f 44 00 00",
                );
            }
            7 => {
                length = append_decimal(&mut self.line_scratch, length, line_number % 256);
                length = append_bytes(
                    &mut self.line_scratch,
                    length,
                    b"] = decode(packet, stream_id);",
                );
            }
            _ => {}
        }
        length
    }
}

fn line_template_index(line_number: u64, pane: usize) -> usize {
    (line_number as usize).wrapping_add(pane.wrapping_mul(3)) % 12
}

fn is_batch_line(template_index: usize) -> bool {
    matches!(template_index, 2..=6)
}

fn pane_count(columns: u32) -> usize {
    (columns / PANE_WIDTH).max(1).min(MAX_PANES as u32) as usize
}

fn pane_bounds(columns: usize, pane: usize, panes: usize) -> (usize, usize) {
    let start = pane * columns / panes;
    let end = (pane + 1) * columns / panes;
    (start, end)
}

fn copy_bytes(dst: &mut [u8], offset: usize, src: &[u8]) -> usize {
    let count = src.len().min(dst.len().saturating_sub(offset));
    dst[offset..offset + count].copy_from_slice(&src[..count]);
    offset + count
}

fn append_bytes(dst: &mut [u8], offset: usize, src: &[u8]) -> usize {
    copy_bytes(dst, offset, src)
}

fn append_decimal(dst: &mut [u8], mut offset: usize, mut value: u64) -> usize {
    let mut digits = [b'0'; 20];
    let mut end = digits.len();
    if value == 0 {
        return copy_bytes(dst, offset, b"0");
    }
    while value != 0 && end > 0 {
        end -= 1;
        digits[end] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    for digit in &digits[end..] {
        if offset >= dst.len() {
            break;
        }
        dst[offset] = *digit;
        offset += 1;
    }
    offset
}

fn append_hex(dst: &mut [u8], mut offset: usize, mut value: u64) -> usize {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut digits = [b'0'; 16];
    let mut end = digits.len();
    if value == 0 {
        return copy_bytes(dst, offset, b"0");
    }
    while value != 0 && end > 0 {
        end -= 1;
        digits[end] = HEX[(value & 0xf) as usize];
        value >>= 4;
    }
    for digit in &digits[end..] {
        if offset >= dst.len() {
            break;
        }
        dst[offset] = *digit;
        offset += 1;
    }
    offset
}

crate::primitive! {
    name: TerminalStream,
    type_id: "node.terminal_stream",
    purpose: "Emit coherent terminal character codes. With reaction wired, local image brightness, contrast and change drive distributed typing, erasing, line growth and code rewrites. Without reaction, emit the original scrolling shell. Text Size controls the 1080p-reference grid; Activity zero freezes cells, repeated beats hold, and rewinds reset deterministically.",
    inputs: {
        canvas: Texture2D required,
        reaction: Texture2D optional,
        text_size: ScalarF32 optional,
        activity: ScalarF32 optional,
    },
    outputs: {
        cells: Channels[VALUE: U32],
        columns: ScalarF32,
        rows: ScalarF32,
    },
    params: [
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
    ],
    depth_rule: Terminal,
    composition_notes: "canvas supplies dimensions; optional reaction supplies the image that drives character layout and content. A fixed 64×36 image analysis is read only after its GPU fence completes, normally one frame later. Reuse bounded storage and hold the last completed analysis if all readback slots are busy. Cells stay printable ASCII plus cursor 127, with fixed 86400-u32 capacity; columns/rows describe the active grid. Palette, glyph rendering and erosion remain downstream graph operations. This CPU readback/upload is an IoBridge fusion boundary.",
    examples: [],
    picker: { label: "Terminal Stream", category: Atom },
    summary: "Types and rewrites readable code across the image, responding to local brightness and motion.",
    category: Generate,
    role: Source,
    aliases: ["terminal", "live terminal", "shell stream", "console source"],
    boundary_reason: IoBridge,
    extra_fields: {
        state: TerminalState = TerminalState::new(),
        reactive: ReactiveTerminal = ReactiveTerminal::new(),
        analysis: TerminalAnalysis = TerminalAnalysis::new(),
        reaction_connected: bool = false,
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
        let dimensions = grid_dimensions(canvas.width, canvas.height, text_size);
        let reaction = ctx.inputs.texture_2d("reaction");
        let beat = ctx.time.beats;
        if reaction.is_some() != self.reaction_connected
            || self.reaction_beat.is_some_and(|previous| beat < previous)
        {
            self.reactive.reset();
            self.analysis.reset();
            self.state.initialized = false;
        }
        self.reaction_connected = reaction.is_some();
        self.reaction_beat = Some(beat);
        let cells = if let Some(image) = reaction {
            let gpu = ctx.gpu_encoder();
            self.analysis.install(gpu.device);
            let samples = self.analysis.sample(gpu, image);
            self.reactive
                .update(dimensions.columns, dimensions.rows, beat, activity, samples);
            self.reactive.cells()
        } else {
            self.state.ensure_dimensions(dimensions);
            self.state.advance(beat, activity);
            self.state.render();
            self.state.cells.as_ref()
        };

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
        self.state.initialized = false;
        self.state.last_beat = None;
        self.reactive.reset();
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
        assert_eq!(pane_count(133), 2);
        assert_eq!(pane_bounds(133, 0, 2), (0, 66));
        assert_eq!(pane_bounds(133, 1, 2), (66, 133));
    }

    #[test]
    fn terminal_state_reset_is_deterministic() {
        let dimensions = GridDimensions {
            columns: 120,
            rows: 5,
        };
        let mut a = TerminalState::new();
        let mut b = TerminalState::new();
        a.reset(dimensions);
        b.reset(dimensions);
        assert_ne!(a.completed_lines[0], a.completed_lines[1]);
        a.advance(Beats(0.0), 1.0);
        b.advance(Beats(0.0), 1.0);
        a.advance(Beats(2.0), 2.0);
        b.advance(Beats(2.0), 2.0);
        a.render();
        b.render();
        assert_eq!(a.cells, b.cells);
        assert_eq!(a.completed_lines, b.completed_lines);
        assert_eq!(a.typing_lengths, b.typing_lengths);
        a.reset(dimensions);
        b.reset(dimensions);
        assert_eq!(a.cells, b.cells);
        assert_eq!(a.character_phases, b.character_phases);
    }

    #[test]
    fn freeze_and_rate_change_preserve_phase() {
        let dimensions = GridDimensions {
            columns: 120,
            rows: 5,
        };
        let mut state = TerminalState::new();
        state.reset(dimensions);
        state.advance(Beats(0.0), 1.0);
        state.advance(Beats(0.5), 1.0);
        let before_typing = state.typing_lengths;
        let before_phase = state.character_phases;
        state.advance(Beats(0.75), 0.0);
        assert_eq!(state.character_phases, before_phase);
        assert_eq!(state.typing_lengths, before_typing);
        state.advance(Beats(1.0), 2.0);
        assert!(
            state
                .typing_lengths
                .iter()
                .zip(before_typing)
                .any(|(a, b)| *a != b)
        );
    }

    #[test]
    fn repeated_beats_are_idempotent_and_rewind_resets() {
        let dimensions = GridDimensions {
            columns: 120,
            rows: 5,
        };
        let mut state = TerminalState::new();
        state.reset(dimensions);
        state.advance(Beats(0.0), 1.0);
        state.advance(Beats(2.0), 1.0);
        state.render();
        let cells = state.cells.clone();
        let lines = state.completed_lines;
        state.advance(Beats(2.0), 4.0);
        state.render();
        assert_eq!(state.cells, cells);
        assert_eq!(state.completed_lines, lines);
        state.advance(Beats(1.0), 1.0);
        let expected_lines = std::array::from_fn(|pane| 1004 + pane as u64 * PANE_HISTORY_OFFSET);
        assert_eq!(state.completed_lines, expected_lines);
        assert_eq!(state.typing_lengths, [INITIAL_TYPING_LENGTH; MAX_PANES]);
    }

    #[test]
    fn frame_subdivision_preserves_typing_and_pause_state() {
        let dimensions = GridDimensions {
            columns: 120,
            rows: 5,
        };
        let mut one_frame = TerminalState::new();
        let mut many_frames = TerminalState::new();
        one_frame.reset(dimensions);
        many_frames.reset(dimensions);
        one_frame.advance(Beats(0.0), 1.0);
        many_frames.advance(Beats(0.0), 1.0);
        one_frame.advance(Beats(2.0), 1.0);
        for frame in 1..=120 {
            many_frames.advance(Beats(frame as f64 * 2.0 / 120.0), 1.0);
        }
        assert_eq!(one_frame.completed_lines, many_frames.completed_lines);
        assert_eq!(one_frame.typing_lengths, many_frames.typing_lengths);
        assert_eq!(one_frame.pause_ticks, many_frames.pause_ticks);
        assert_eq!(one_frame.character_phases, many_frames.character_phases);
    }

    #[test]
    fn cursor_uses_full_block_and_freezes_with_activity() {
        let dimensions = GridDimensions {
            columns: 120,
            rows: 5,
        };
        let mut state = TerminalState::new();
        state.reset(dimensions);
        state.render();
        assert!(state.cells.contains(&127));
        state.advance(Beats(0.0), 1.0);
        state.advance(Beats(0.25), 1.0);
        state.render();
        assert!(state.cells.iter().all(|cell| *cell != 127));
        let frozen_cells = state.cells.clone();
        state.advance(Beats(0.75), 0.0);
        state.render();
        assert_eq!(state.cells.as_ref(), frozen_cells.as_ref());
    }

    #[test]
    fn typing_burst_and_scroll_keep_visible_cells_populated() {
        let dimensions = GridDimensions {
            columns: 120,
            rows: 4,
        };
        let mut state = TerminalState::new();
        state.reset(dimensions);
        state.render();
        let initial = state.cells[..(dimensions.columns * dimensions.rows) as usize].to_vec();
        assert!(initial.iter().any(|cell| *cell != u32::from(b' ')));
        state.advance(Beats(0.0), 1.0);
        state.advance(Beats(20.0), 4.0);
        state.render();
        assert!(state.completed_lines.iter().any(|line| *line > 1003));
        assert!(
            state.cells[..(dimensions.columns * dimensions.rows) as usize]
                .iter()
                .any(|cell| *cell == u32::from(b'$'))
        );
    }

    #[test]
    fn declares_canvas_and_typed_cells_contract() {
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(TerminalStream::TYPE_ID, "node.terminal_stream");
        assert_eq!(TerminalStream::INPUTS.len(), 4);
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
