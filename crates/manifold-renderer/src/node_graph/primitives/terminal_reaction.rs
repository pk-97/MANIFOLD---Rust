// Bounded CPU state for an image-reactive terminal source.
//
// The GPU readback and node wiring live beside this module.  This file owns
// only the fixed-size state machine which turns a small image summary into
// independently progressing terminal segments.

use manifold_core::Beats;

pub(super) const SAMPLE_COLS: usize = 64;
pub(super) const SAMPLE_ROWS: usize = 36;
pub(super) const SAMPLE_COUNT: usize = SAMPLE_COLS * SAMPLE_ROWS;

const MAX_COLS: usize = 640;
const MAX_ROWS: usize = 135;
const MAX_CELLS: usize = MAX_COLS * MAX_ROWS;
const SEGMENT_WIDTH: usize = 32;
const MAX_SEGMENTS: usize = MAX_COLS.div_ceil(SEGMENT_WIDTH);
const MAX_STATES: usize = MAX_ROWS * MAX_SEGMENTS;
const MAX_ADVANCE_BEATS: f64 = 8.0;
const MAX_STEPS_PER_STATE: usize = 512;
const SOURCE_EPSILON: f32 = 0.035;
const REWRITE_COOLDOWN_BEATS: f64 = 0.35;

/// Fixed-size image-reactive terminal state.
///
/// One state slot represents one row in one roughly 32-character segment.
/// All arrays are sized for the largest supported grid, so changing the
/// dimensions never allocates and the update path remains bounded.
pub(super) struct ReactiveTerminal {
    cells: Box<[u32]>,
    line_ids: [u64; MAX_STATES],
    visible_lengths: [u16; MAX_STATES],
    fractional_steps: [f64; MAX_STATES],
    hold_steps: [u16; MAX_STATES],
    modes: [u8; MAX_STATES],
    reaction: [f32; MAX_STATES],
    brightness: [f32; MAX_STATES],
    contrast: [f32; MAX_STATES],
    pending_brightness: [f32; MAX_STATES],
    pending_contrast: [f32; MAX_STATES],
    pending: [bool; MAX_STATES],
    rewrite_cooldown: [f64; MAX_STATES],
    seen: [bool; MAX_STATES],
    columns: usize,
    rows: usize,
    last_beat: Option<f64>,
}

impl ReactiveTerminal {
    pub(super) fn new() -> Self {
        Self {
            cells: vec![b' ' as u32; MAX_CELLS].into_boxed_slice(),
            line_ids: [0; MAX_STATES],
            visible_lengths: [0; MAX_STATES],
            fractional_steps: [0.0; MAX_STATES],
            hold_steps: [0; MAX_STATES],
            modes: [0; MAX_STATES],
            reaction: [0.0; MAX_STATES],
            brightness: [0.0; MAX_STATES],
            contrast: [0.0; MAX_STATES],
            pending_brightness: [0.0; MAX_STATES],
            pending_contrast: [0.0; MAX_STATES],
            pending: [false; MAX_STATES],
            rewrite_cooldown: [0.0; MAX_STATES],
            seen: [false; MAX_STATES],
            columns: 0,
            rows: 0,
            last_beat: None,
        }
    }

    /// Return to the deterministic initial state without releasing storage.
    pub(super) fn reset(&mut self) {
        self.cells.fill(b' ' as u32);
        self.line_ids.fill(0);
        self.visible_lengths.fill(0);
        self.fractional_steps.fill(0.0);
        self.hold_steps.fill(0);
        self.modes.fill(0);
        self.reaction.fill(0.0);
        self.brightness.fill(0.0);
        self.contrast.fill(0.0);
        self.pending_brightness.fill(0.0);
        self.pending_contrast.fill(0.0);
        self.pending.fill(false);
        self.rewrite_cooldown.fill(0.0);
        self.seen.fill(false);
        self.columns = 0;
        self.rows = 0;
        self.last_beat = None;
    }

    /// Advance and render the terminal for one beat position.
    ///
    /// Samples are `[linear_r, linear_g, linear_b, local_contrast]`.  Invalid
    /// values are ignored and all usable values are clamped before affecting
    /// the state machine or its numeric fields.
    pub(super) fn update(
        &mut self,
        columns: u32,
        rows: u32,
        beat: Beats,
        activity: f32,
        samples: &[[f32; 4]; SAMPLE_COUNT],
    ) {
        let columns = (columns as usize).clamp(1, MAX_COLS);
        let rows = (rows as usize).clamp(1, MAX_ROWS);
        let beat = finite_beat(beat.0).unwrap_or_else(|| self.last_beat.unwrap_or(0.0));
        let activity = if activity.is_finite() {
            activity.clamp(0.0, 4.0)
        } else {
            0.0
        };
        let was_initialized = self.last_beat.is_some();
        let dimensions_changed = self.columns != columns || self.rows != rows;

        self.columns = columns;
        self.rows = rows;

        let Some(previous_beat) = self.last_beat else {
            self.last_beat = Some(beat);
            self.capture_source(columns, rows, samples, false);
            self.render();
            return;
        };

        if beat < previous_beat {
            self.reset();
            self.columns = columns;
            self.rows = rows;
            self.last_beat = Some(beat);
            self.capture_source(columns, rows, samples, false);
            self.render();
            return;
        }

        if dimensions_changed {
            self.reset();
            self.columns = columns;
            self.rows = rows;
            self.last_beat = Some(beat);
            self.capture_source(columns, rows, samples, false);
            self.render();
            return;
        }

        // Equal beats are deliberately a no-op.  This makes repeated graph
        // evaluations idempotent, including when their readback differs.
        if beat == previous_beat {
            return;
        }
        if activity <= 0.0 {
            // Advance the reference point while frozen so resuming activity
            // does not apply a large catch-up burst for the paused interval.
            self.last_beat = Some(beat);
            return;
        }

        self.last_beat = Some(beat);
        let delta = (beat - previous_beat).clamp(0.0, MAX_ADVANCE_BEATS);
        self.capture_source(columns, rows, samples, was_initialized);
        self.advance(delta, activity);
        self.render();
    }

    pub(super) fn cells(&self) -> &[u32] {
        let active = self.columns.saturating_mul(self.rows).min(MAX_CELLS);
        &self.cells[..active]
    }

    fn capture_source(
        &mut self,
        columns: usize,
        rows: usize,
        samples: &[[f32; 4]; SAMPLE_COUNT],
        detect_changes: bool,
    ) {
        let segments = columns.div_ceil(SEGMENT_WIDTH).min(MAX_SEGMENTS);
        for row in 0..rows {
            let sy0 = row * SAMPLE_ROWS / rows;
            let sy1 = ((row + 1) * SAMPLE_ROWS / rows)
                .max(sy0 + 1)
                .min(SAMPLE_ROWS);
            for segment in 0..segments {
                let x0 = segment * SEGMENT_WIDTH;
                let x1 = ((segment + 1) * SEGMENT_WIDTH).min(columns);
                let sx0 = x0 * SAMPLE_COLS / columns;
                let sx1 = ((x1 * SAMPLE_COLS / columns).max(sx0 + 1)).min(SAMPLE_COLS);
                let (brightness, contrast) = source_feature(samples, sx0, sx1, sy0, sy1);
                let state = row * MAX_SEGMENTS + segment;
                if !self.seen[state] {
                    self.seen[state] = true;
                    self.brightness[state] = brightness;
                    self.contrast[state] = contrast;
                    self.pending_brightness[state] = brightness;
                    self.pending_contrast[state] = contrast;
                    self.line_ids[state] = (state as u64).wrapping_mul(17).wrapping_add(1);
                    let target = line_length(
                        self.line_ids[state],
                        segment,
                        row,
                        brightness,
                        contrast,
                        x1 - x0,
                    );
                    let stagger = ((state as u32).wrapping_mul(29) % 11) as usize;
                    self.visible_lengths[state] = target.min(stagger as u16);
                    self.modes[state] = if self.visible_lengths[state] >= target {
                        1
                    } else {
                        0
                    };
                    self.fractional_steps[state] = f64::from(stagger as u16) * 0.13;
                } else if detect_changes {
                    self.pending_brightness[state] = brightness;
                    self.pending_contrast[state] = contrast;
                    let changed = (brightness - self.brightness[state]).abs() > SOURCE_EPSILON
                        || (contrast - self.contrast[state]).abs() > SOURCE_EPSILON;
                    if changed {
                        self.pending[state] = true;
                        self.reaction[state] = 1.0;
                    } else if self.pending[state] {
                        self.pending[state] = false;
                    }
                    if self.pending[state]
                        && self.modes[state] == 1
                        && self.rewrite_cooldown[state] <= 0.0
                    {
                        self.begin_rewrite(state);
                    }
                }
            }
        }
    }

    fn begin_rewrite(&mut self, state: usize) {
        if !self.pending[state] {
            return;
        }
        self.reaction[state] = 1.0;
        self.rewrite_cooldown[state] = REWRITE_COOLDOWN_BEATS;
        if self.visible_lengths[state] == 0 {
            self.apply_pending(state);
            self.modes[state] = 0;
        } else {
            self.modes[state] = 2;
            self.hold_steps[state] = 0;
        }
    }

    fn apply_pending(&mut self, state: usize) {
        if !self.pending[state] {
            return;
        }
        self.brightness[state] = self.pending_brightness[state];
        self.contrast[state] = self.pending_contrast[state];
        self.pending[state] = false;
    }

    fn advance(&mut self, delta: f64, activity: f32) {
        let segments = self.columns.div_ceil(SEGMENT_WIDTH).min(MAX_SEGMENTS);
        for row in 0..self.rows {
            for segment in 0..segments {
                let state = row * MAX_SEGMENTS + segment;
                let width =
                    ((segment + 1) * SEGMENT_WIDTH).min(self.columns) - segment * SEGMENT_WIDTH;
                let base_rate = 10.0
                    + f64::from(self.brightness[state]) * 7.0
                    + f64::from(self.contrast[state]) * 5.0;
                // Independent row speeds prevent neighbouring cursors from
                // forming a solid vertical bar over a flat source region.
                let row_rate = 0.8 + ((state * 37 + 11) % 17) as f64 * 0.025;
                let rate = base_rate
                    * row_rate
                    * f64::from(activity)
                    * (1.0 + f64::from(self.reaction[state]) * 4.0);
                let exact_steps = self.fractional_steps[state] + delta * rate;
                let mut steps = exact_steps.floor().max(0.0) as usize;
                self.fractional_steps[state] = exact_steps - steps as f64;
                steps = steps.min(MAX_STEPS_PER_STATE);
                for _ in 0..steps {
                    let target = line_length(
                        self.line_ids[state],
                        segment,
                        row,
                        self.brightness[state],
                        self.contrast[state],
                        width,
                    );
                    match self.modes[state] {
                        0 => {
                            if usize::from(self.visible_lengths[state]) < usize::from(target) {
                                self.visible_lengths[state] += 1;
                            } else {
                                self.modes[state] = 1;
                                self.hold_steps[state] = 0;
                            }
                        }
                        1 => {
                            self.hold_steps[state] = self.hold_steps[state].saturating_add(1);
                            let hold = 8 + ((1.0 - self.brightness[state]) * 8.0).round() as u16;
                            if self.hold_steps[state] >= hold {
                                self.modes[state] = 2;
                                self.hold_steps[state] = 0;
                            }
                        }
                        _ => {
                            if self.visible_lengths[state] != 0 {
                                self.visible_lengths[state] -= 1;
                            } else {
                                self.line_ids[state] = self.line_ids[state].wrapping_add(1);
                                self.apply_pending(state);
                                self.modes[state] = 0;
                                self.hold_steps[state] = 0;
                            }
                        }
                    }
                }
                self.reaction[state] =
                    (self.reaction[state] - (delta as f32 * 0.75).clamp(0.0, 1.0)).max(0.0);
                self.rewrite_cooldown[state] = (self.rewrite_cooldown[state] - delta).max(0.0);
                if self.pending[state]
                    && self.modes[state] == 1
                    && self.rewrite_cooldown[state] <= 0.0
                {
                    self.begin_rewrite(state);
                }
            }
        }
    }

    fn render(&mut self) {
        let segments = self.columns.div_ceil(SEGMENT_WIDTH).min(MAX_SEGMENTS);
        let active = self.columns.saturating_mul(self.rows).min(MAX_CELLS);
        self.cells[..active].fill(b' ' as u32);
        for row in 0..self.rows {
            for segment in 0..segments {
                let x0 = segment * SEGMENT_WIDTH;
                let width = ((segment + 1) * SEGMENT_WIDTH).min(self.columns) - x0;
                let state = row * MAX_SEGMENTS + segment;
                let mut line = [b' '; 64];
                let length = make_line(
                    &mut line,
                    self.line_ids[state],
                    segment,
                    row,
                    self.brightness[state],
                    self.contrast[state],
                )
                .min(width);
                let visible = usize::from(self.visible_lengths[state]).min(width);
                for (local_x, &character) in line.iter().take(width).enumerate() {
                    let index = row * self.columns + x0 + local_x;
                    self.cells[index] =
                        if self.modes[state] != 1 && local_x == visible && visible < width {
                            127
                        } else if local_x < visible.min(length) {
                            u32::from(character)
                        } else {
                            b' ' as u32
                        };
                }
            }
        }
    }
}

fn finite_beat(value: f64) -> Option<f64> {
    value.is_finite().then_some(value.max(0.0))
}

fn source_feature(
    samples: &[[f32; 4]; SAMPLE_COUNT],
    sx0: usize,
    sx1: usize,
    sy0: usize,
    sy1: usize,
) -> (f32, f32) {
    let mut luma = 0.0_f32;
    let mut contrast = 0.0_f32;
    let mut count = 0.0_f32;
    for y in sy0..sy1 {
        for x in sx0..sx1 {
            let sample = samples[y * SAMPLE_COLS + x];
            let r = finite_unit(sample[0]);
            let g = finite_unit(sample[1]);
            let b = finite_unit(sample[2]);
            luma += 0.2126 * r + 0.7152 * g + 0.0722 * b;
            contrast += finite_unit(sample[3]);
            count += 1.0;
        }
    }
    if count == 0.0 {
        return (0.0, 0.0);
    }
    (
        (luma / count).clamp(0.0, 1.0),
        (contrast / count).clamp(0.0, 1.0),
    )
}

fn finite_unit(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn line_length(
    line_id: u64,
    segment: usize,
    row: usize,
    brightness: f32,
    contrast: f32,
    width: usize,
) -> u16 {
    let mut line = [b' '; 64];
    let generated = make_line(&mut line, line_id, segment, row, brightness, contrast).min(width);
    // Shadows retain enough of a statement to read as code. Source changes
    // extend that statement; they do not reduce every line to a few letters.
    let density = (0.45 + brightness * 0.45 + contrast * 0.10).clamp(0.45, 1.0);
    let budget = if width <= 3 {
        width
    } else {
        3 + (((width - 3) as f32) * density).round() as usize
    };
    generated.min(budget).max(generated.min(width).min(3)) as u16
}

fn make_line(
    dst: &mut [u8; 64],
    line_id: u64,
    segment: usize,
    row: usize,
    brightness: f32,
    contrast: f32,
) -> usize {
    dst.fill(b' ');
    let template = (line_id as usize + segment * 3 + row) % 8;
    let indent = ((brightness * 4.0).round() as usize).min(4);
    let mut offset = 0;
    for _ in 0..indent {
        offset = append_bytes(dst, offset, b"  ");
    }
    match template {
        0 => {
            offset = append_bytes(dst, offset, b"$ render lane=");
            offset = append_decimal(dst, offset, segment as u64);
            offset = append_bytes(dst, offset, b" gain=");
            append_thousand(dst, offset, brightness)
        }
        1 => {
            offset = append_bytes(dst, offset, b"const luma = ");
            offset = append_thousand(dst, offset, brightness);
            append_bytes(dst, offset, b";")
        }
        2 => {
            offset = append_bytes(dst, offset, b"edge += ");
            offset = append_thousand(dst, offset, contrast);
            append_bytes(dst, offset, b"; // local")
        }
        3 => append_bytes(dst, offset, b"if (luma > 0.50) draw();"),
        4 => append_bytes(dst, offset, b"mix(buffer, patch, gain);"),
        5 => {
            offset = append_bytes(dst, offset, b"await sync(row=");
            offset = append_decimal(dst, offset, row as u64);
            append_bytes(dst, offset, b");")
        }
        6 => append_bytes(dst, offset, b"pixels[i] *= gain;"),
        _ => {
            offset = append_bytes(dst, offset, b"frame=");
            offset = append_decimal(dst, offset, line_id % 100_000);
            append_bytes(dst, offset, b" ready")
        }
    }
}

fn append_bytes(dst: &mut [u8; 64], offset: usize, src: &[u8]) -> usize {
    let count = src.len().min(dst.len().saturating_sub(offset));
    dst[offset..offset + count].copy_from_slice(&src[..count]);
    offset + count
}

fn append_decimal(dst: &mut [u8; 64], mut offset: usize, mut value: u64) -> usize {
    let mut digits = [b'0'; 20];
    let mut end = digits.len();
    if value == 0 {
        return append_bytes(dst, offset, b"0");
    }
    while value != 0 && end != 0 {
        end -= 1;
        digits[end] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    for digit in &digits[end..] {
        if offset == dst.len() {
            break;
        }
        dst[offset] = *digit;
        offset += 1;
    }
    offset
}

fn append_thousand(dst: &mut [u8; 64], mut offset: usize, value: f32) -> usize {
    let scaled = (finite_unit(value) * 1000.0).round() as u64;
    if offset == dst.len() {
        return offset;
    }
    dst[offset] = b'0' + (scaled / 1000) as u8;
    offset += 1;
    if offset == dst.len() {
        return offset;
    }
    dst[offset] = b'.';
    offset += 1;
    let fraction = scaled % 1000;
    for divisor in [100, 10, 1] {
        if offset == dst.len() {
            break;
        }
        dst[offset] = b'0' + ((fraction / divisor) % 10) as u8;
        offset += 1;
    }
    offset
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples(brightness: f32, contrast: f32) -> [[f32; 4]; SAMPLE_COUNT] {
        [[brightness, brightness, brightness, contrast]; SAMPLE_COUNT]
    }

    #[test]
    fn bright_and_dark_sources_change_layout() {
        let mut terminal = ReactiveTerminal::new();
        let dark = samples(0.05, 0.05);
        let bright = samples(0.95, 0.85);
        terminal.update(64, 12, Beats(0.0), 1.0, &dark);
        let dark_cells = terminal.cells().to_vec();
        terminal.update(64, 12, Beats(1.0), 1.0, &bright);
        assert_ne!(dark_cells, terminal.cells());
        assert!(
            terminal
                .cells()
                .iter()
                .all(|cell| (32..=127).contains(cell))
        );
    }

    #[test]
    fn line_density_follows_brightness_and_contrast() {
        let dark = line_length(1, 0, 0, 0.05, 0.05, 32);
        let bright = line_length(1, 0, 0, 0.95, 0.85, 32);
        assert!(dark >= 12, "shadows retain a readable code fragment");
        assert!(bright > dark);
        assert!(bright <= 32);
    }

    #[test]
    fn local_patch_changes_its_rows_with_equal_global_mean() {
        let mut terminal = ReactiveTerminal::new();
        let mut first = samples(0.5, 0.1);
        let mut second = first;
        for row in 12..18 {
            for col in 24..32 {
                first[row * SAMPLE_COLS + col] = [0.1, 0.1, 0.1, 0.1];
                second[row * SAMPLE_COLS + col] = [0.9, 0.9, 0.9, 0.9];
            }
        }
        for row in 24..30 {
            for col in 40..48 {
                first[row * SAMPLE_COLS + col] = [0.9, 0.9, 0.9, 0.9];
                second[row * SAMPLE_COLS + col] = [0.1, 0.1, 0.1, 0.1];
            }
        }
        terminal.update(128, 36, Beats(0.0), 1.0, &first);
        let before = terminal.cells().to_vec();
        terminal.update(128, 36, Beats(1.0), 1.0, &second);
        let after = terminal.cells();
        let changed_patch_rows = (12..18).any(|row| {
            let start = row * 128 + 32;
            before[start..start + 32] != after[start..start + 32]
        });
        let changed_elsewhere = (0..12).any(|row| {
            let start = row * 128;
            before[start..start + 128] != after[start..start + 128]
        });
        assert!(changed_patch_rows);
        assert!(changed_elsewhere);
    }

    #[test]
    fn animation_is_distributed_away_from_bottom() {
        let mut terminal = ReactiveTerminal::new();
        let source = samples(0.4, 0.3);
        terminal.update(96, 30, Beats(0.0), 1.0, &source);
        let first = terminal.cells().to_vec();
        terminal.update(96, 30, Beats(0.5), 1.0, &source);
        let second = terminal.cells();
        assert!(first[..96 * 10] != second[..96 * 10]);
    }

    #[test]
    fn same_beat_resize_reinitializes_the_active_grid() {
        let mut terminal = ReactiveTerminal::new();
        let source = samples(0.4, 0.3);
        terminal.update(16, 2, Beats(0.0), 1.0, &source);
        terminal.update(8, 3, Beats(0.0), 1.0, &source);
        assert_eq!(terminal.cells().len(), 24);
        assert!(
            terminal
                .cells()
                .iter()
                .all(|cell| (32..=127).contains(cell))
        );
    }

    #[test]
    fn changing_source_still_completes_useful_lines() {
        let mut terminal = ReactiveTerminal::new();
        let mut longest_statement = 0;
        for frame in 0..=30 {
            let brightness = if frame % 2 == 0 { 0.15 } else { 0.85 };
            let source = samples(brightness, 0.5);
            terminal.update(64, 8, Beats(f64::from(frame) * 0.1), 1.0, &source);
            for segment in terminal.cells().chunks(SEGMENT_WIDTH) {
                let ink = segment
                    .iter()
                    .filter(|&&cell| (33..=126).contains(&cell))
                    .count();
                longest_statement = longest_statement.max(ink);
            }
        }
        assert!(
            longest_statement >= 12,
            "motion must allow readable statements to finish"
        );
    }

    #[test]
    fn activity_zero_freezes_even_when_source_changes() {
        let mut terminal = ReactiveTerminal::new();
        let first = samples(0.2, 0.1);
        let second = samples(0.9, 0.8);
        terminal.update(64, 8, Beats(0.0), 1.0, &first);
        let before = terminal.cells().to_vec();
        terminal.update(64, 8, Beats(4.0), 0.0, &second);
        assert_eq!(before, terminal.cells());
    }

    #[test]
    fn reset_and_repeated_beats_are_deterministic() {
        let source = samples(0.4, 0.2);
        let mut first = ReactiveTerminal::new();
        let mut second = ReactiveTerminal::new();
        first.update(80, 10, Beats(0.0), 1.0, &source);
        first.update(80, 10, Beats(2.0), 1.0, &source);
        let cells = first.cells().to_vec();
        first.update(80, 10, Beats(2.0), 4.0, &samples(0.9, 0.9));
        assert_eq!(cells, first.cells());
        first.reset();
        first.update(80, 10, Beats(0.0), 1.0, &source);
        first.update(80, 10, Beats(2.0), 1.0, &source);
        second.update(80, 10, Beats(0.0), 1.0, &source);
        second.update(80, 10, Beats(2.0), 1.0, &source);
        assert_eq!(first.cells(), second.cells());
        first.update(80, 10, Beats(1.0), 1.0, &source);
        second.update(80, 10, Beats(1.0), 1.0, &source);
        assert_eq!(first.cells(), second.cells());
    }

    #[test]
    fn finite_extremes_are_clamped_and_bounded() {
        let mut terminal = ReactiveTerminal::new();
        let mut source = samples(f32::NAN, f32::INFINITY);
        source[0] = [f32::NEG_INFINITY, 4.0, -2.0, f32::NAN];
        terminal.update(u32::MAX, u32::MAX, Beats(f64::NAN), f32::INFINITY, &source);
        assert_eq!(terminal.cells().len(), MAX_CELLS);
        assert!(
            terminal
                .cells()
                .iter()
                .all(|cell| (32..=127).contains(cell))
        );
        terminal.update(1, 1, Beats(f64::MAX), 2.0, &source);
        assert_eq!(terminal.cells().len(), 1);
    }
}
