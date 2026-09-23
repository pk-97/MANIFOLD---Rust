//! Source-driven terminal lines. Completed text is stationary; a bounded
//! scheduler types only changed spans when the image's spatial profile changes.
use manifold_core::Beats;

pub(super) const SAMPLE_COLS: usize = 64;
pub(super) const SAMPLE_ROWS: usize = 36;
pub(super) const SAMPLE_COUNT: usize = SAMPLE_COLS * SAMPLE_ROWS;
pub(super) static EMPTY_SAMPLES: [[f32; 4]; SAMPLE_COUNT] = [[0.0; 4]; SAMPLE_COUNT];
const MAX_COLS: usize = 640;
const MAX_ROWS: usize = 135;
const MAX_LINES: usize = MAX_ROWS * 2;
const MAX_ACTIVE: usize = 3;
const CHANGE_THRESHOLD: f32 = 0.025;
type Profile = [[f32; 4]; SAMPLE_COLS];

#[derive(Clone, Copy, Default)]
struct Region {
    x: usize,
    y: usize,
    width: usize,
    pane: usize,
    row: usize,
}

#[derive(Clone, Copy, Default)]
struct Features {
    light: f32,
    edge: f32,
    center: f32,
    span: f32,
}

#[derive(Clone, Copy)]
struct Line {
    region: Region,
    shown: [u8; MAX_COLS],
    target: [u8; MAX_COLS],
    reference: Profile,
    latest: Profile,
    features: Features,
    score: f32,
    cursor: usize,
    end: usize,
    active: bool,
    phase: f64,
    eligible_at: f64,
    changed_at: f64,
}

impl Line {
    fn new() -> Self {
        Self {
            region: Region::default(),
            shown: [b' '; MAX_COLS],
            target: [b' '; MAX_COLS],
            reference: [[0.0; 4]; SAMPLE_COLS],
            latest: [[0.0; 4]; SAMPLE_COLS],
            features: Features::default(),
            score: 0.0,
            cursor: 0,
            end: 0,
            active: false,
            phase: 0.0,
            eligible_at: 0.0,
            changed_at: 0.0,
        }
    }
}

/// All text, profiles and scheduling storage is allocated once. Layout changes
/// reuse it. There are at most two text lines per screen row (four-pane layout).
pub(super) struct ReactiveTerminal {
    cells: Box<[u32]>,
    lines: Box<[Line]>,
    line_count: usize,
    columns: usize,
    rows: usize,
    layout: u8,
    last_beat: Option<f64>,
    clock: f64,
    next_start: f64,
}

impl ReactiveTerminal {
    pub(super) fn new() -> Self {
        Self {
            cells: vec![u32::from(b' '); MAX_COLS * MAX_ROWS].into_boxed_slice(),
            lines: vec![Line::new(); MAX_LINES].into_boxed_slice(),
            line_count: 0,
            columns: 0,
            rows: 0,
            layout: 0,
            last_beat: None,
            clock: 0.0,
            next_start: 0.0,
        }
    }

    pub(super) fn reset(&mut self) {
        self.last_beat = None;
        self.line_count = 0;
        self.clock = 0.0;
        self.next_start = 0.0;
    }

    pub(super) fn update(
        &mut self,
        columns: u32,
        rows: u32,
        layout: u8,
        beat: Beats,
        activity: f32,
        samples: &[[f32; 4]; SAMPLE_COUNT],
    ) {
        let columns = (columns as usize).clamp(1, MAX_COLS);
        let rows = (rows as usize).clamp(1, MAX_ROWS);
        let layout = layout.min(3);
        let beat = if beat.0.is_finite() {
            beat.0.max(0.0)
        } else {
            self.last_beat.unwrap_or(0.0)
        };
        if self.last_beat.is_none()
            || self.last_beat.is_some_and(|previous| beat < previous)
            || (columns, rows, layout) != (self.columns, self.rows, self.layout)
        {
            self.reset();
            self.columns = columns;
            self.rows = rows;
            self.layout = layout;
            self.last_beat = Some(beat);
            self.build_layout();
            for line in &mut self.lines[..self.line_count] {
                observe(line, columns, rows, samples);
                line.reference = line.latest;
                make_line(&mut line.shown, line.region, line.features);
            }
            self.render();
            return;
        }
        let previous = self.last_beat.replace(beat).expect("initialized terminal");
        if beat == previous || !activity.is_finite() || activity <= 0.0 {
            return;
        }
        let activity = f64::from(activity.min(4.0));
        let delta = (beat - previous).clamp(0.0, 8.0);
        self.clock += delta;
        let mut active = 0;
        for line in &mut self.lines[..self.line_count] {
            observe(line, columns, rows, samples);
            let score = profile_change(&line.reference, &line.latest);
            if score >= CHANGE_THRESHOLD && line.score < CHANGE_THRESHOLD {
                line.changed_at = self.clock;
            }
            line.score = score;
            if line.active {
                // Only the changed span advances. Existing text outside it
                // remains intact, including completed neighbouring lines.
                let exact = line.phase + delta * activity * 64.0;
                let steps = (exact.floor() as usize).min(MAX_COLS);
                line.phase = exact.fract();
                let end = (line.cursor + steps).min(line.end);
                line.shown[line.cursor..end].copy_from_slice(&line.target[line.cursor..end]);
                line.cursor = end;
                if end == line.end {
                    line.active = false;
                    line.phase = 0.0;
                    line.eligible_at = self.clock + 0.5 / activity;
                } else {
                    active += 1;
                }
            }
        }
        // Source salience wins; aging prevents a quieter changed line from
        // starving behind a continuously moving foreground. No idle jobs.
        if active < MAX_ACTIVE && self.clock >= self.next_start {
            let candidate = self.lines[..self.line_count]
                .iter()
                .enumerate()
                .filter(|(_, line)| {
                    !line.active && line.score >= CHANGE_THRESHOLD && self.clock >= line.eligible_at
                })
                .max_by(|(_, a), (_, b)| {
                    let priority =
                        |line: &Line| line.score + ((self.clock - line.changed_at) * 0.002) as f32;
                    priority(a).total_cmp(&priority(b))
                })
                .map(|(index, _)| index);
            if let Some(index) = candidate {
                let line = &mut self.lines[index];
                make_line(&mut line.target, line.region, line.features);
                let width = line.region.width;
                let first = (0..width).find(|&x| line.shown[x] != line.target[x]);
                line.reference = line.latest;
                line.score = 0.0;
                if let Some(first) = first {
                    line.cursor = first;
                    line.end = (first..width)
                        .rfind(|&x| line.shown[x] != line.target[x])
                        .expect("changed span")
                        + 1;
                    line.active = true;
                    line.phase = 0.0;
                    self.next_start = self.clock + 0.18 / activity;
                }
            }
        }
        self.render();
    }

    pub(super) fn cells(&self) -> &[u32] {
        &self.cells[..self.columns * self.rows]
    }

    fn add_pane(
        &mut self,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        pane: usize,
        framed: bool,
    ) {
        let inset = usize::from(framed);
        let left = x + inset.max(usize::from(width > 2));
        let text_width = width.saturating_sub(2 * inset.max(usize::from(width > 2)));
        if text_width == 0 {
            return;
        }
        for row in 0..height.saturating_sub(2 * inset) {
            let line = &mut self.lines[self.line_count];
            *line = Line::new();
            line.region = Region {
                x: left,
                y: y + inset + row,
                width: text_width,
                pane,
                row,
            };
            self.line_count += 1;
        }
    }

    fn build_layout(&mut self) {
        self.line_count = 0;
        let w = self.columns;
        let h = self.rows;
        // Tiny canvases cannot hold a split plus both interiors. They retain
        // a single visible line rather than constructing empty panes.
        let layout = if w < 7 || h < 5 { 0 } else { self.layout };
        let mx = w / 2;
        let my = h / 2;
        match layout {
            1 => {
                self.add_pane(0, 0, mx + 1, h, 0, true);
                self.add_pane(mx, 0, w - mx, h, 1, true);
            }
            2 => {
                self.add_pane(0, 0, w, my + 1, 0, true);
                self.add_pane(0, my, w, h - my, 1, true);
            }
            3 => {
                self.add_pane(0, 0, mx + 1, my + 1, 0, true);
                self.add_pane(mx, 0, w - mx, my + 1, 1, true);
                self.add_pane(0, my, mx + 1, h - my, 2, true);
                self.add_pane(mx, my, w - mx, h - my, 3, true);
            }
            _ => self.add_pane(0, 0, w, h, 0, false),
        }
    }

    fn render(&mut self) {
        let count = self.columns * self.rows;
        self.cells[..count].fill(u32::from(b' '));
        if self.layout != 0 && self.columns >= 7 && self.rows >= 5 {
            let w = self.columns;
            let h = self.rows;
            let vertical = self.layout == 1 || self.layout == 3;
            let horizontal = self.layout == 2 || self.layout == 3;
            for y in 0..h {
                for x in 0..w {
                    let v = x == 0 || x + 1 == w || (vertical && x == w / 2);
                    let hz = y == 0 || y + 1 == h || (horizontal && y == h / 2);
                    self.cells[y * w + x] = u32::from(match (v, hz) {
                        (true, true) => b'+',
                        (true, false) => b'|',
                        (false, true) => b'-',
                        _ => b' ',
                    });
                }
            }
            let headers: [&[u8]; 4] = [b" 0: shell ", b" 1: trace ", b" 2: edges ", b" 3: source "];
            for line in &self.lines[..self.line_count] {
                if line.region.row == 0 {
                    let region = line.region;
                    for (x, &ch) in headers[region.pane].iter().take(region.width).enumerate() {
                        self.cells[(region.y - 1) * w + region.x + x] = u32::from(ch);
                    }
                }
            }
        }
        for line in &self.lines[..self.line_count] {
            let region = line.region;
            let offset = region.y * self.columns + region.x;
            for (x, &ch) in line.shown[..region.width].iter().enumerate() {
                self.cells[offset + x] = if line.active && x == line.cursor {
                    127
                } else {
                    u32::from(ch)
                };
            }
        }
    }
}

fn unit(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn observe(line: &mut Line, columns: usize, rows: usize, samples: &[[f32; 4]; SAMPLE_COUNT]) {
    let region = line.region;
    let y0 = region.y * SAMPLE_ROWS / rows;
    let y1 = ((region.y + 1) * SAMPLE_ROWS / rows)
        .max(y0 + 1)
        .min(SAMPLE_ROWS);
    for (i, output) in line.latest.iter_mut().enumerate() {
        let x = ((region.x * SAMPLE_COLS + region.width * i) / columns).min(SAMPLE_COLS - 1);
        *output = [0.0; 4];
        for y in y0..y1 {
            for (c, component) in output.iter_mut().enumerate() {
                *component += unit(samples[y * SAMPLE_COLS + x][c]);
            }
        }
        for component in output {
            *component /= (y1 - y0) as f32;
        }
    }
    line.features = features(&line.latest);
}

fn luma(sample: [f32; 4]) -> f32 {
    sample[0] * 0.2126 + sample[1] * 0.7152 + sample[2] * 0.0722
}

fn features(profile: &Profile) -> Features {
    let mut light = 0.0_f32;
    let mut edge = 0.0_f32;
    let mut weight = 0.0_f32;
    let mut moment = 0.0_f32;
    let mut peak = 0.0_f32;
    for (i, &sample) in profile.iter().enumerate() {
        let value = luma(sample);
        let gradient = (value - luma(profile[i.saturating_sub(1)])).abs();
        let structure = gradient + sample[3];
        light += value;
        edge = edge.max(structure);
        peak = peak.max(value);
        let mass = value + structure * 2.0;
        weight += mass;
        moment += mass * (i as f32 + 0.5) / SAMPLE_COLS as f32;
    }
    let threshold = (peak * 0.45).max(0.025);
    let first = profile.iter().position(|&s| luma(s) >= threshold);
    let last = profile.iter().rposition(|&s| luma(s) >= threshold);
    let span = match (first, last) {
        (Some(first), Some(last)) => (last - first + 1) as f32 / SAMPLE_COLS as f32,
        _ => 0.0,
    };
    Features {
        light: light / SAMPLE_COLS as f32,
        edge: edge.min(1.0),
        center: if weight > 0.001 { moment / weight } else { 0.5 },
        span,
    }
}

fn profile_change(a: &Profile, b: &Profile) -> f32 {
    let mut sum = 0.0;
    for (a, b) in a.iter().zip(b) {
        for (a, b) in a.iter().zip(b) {
            sum += (a - b) * (a - b);
        }
    }
    (sum / (SAMPLE_COLS * 4) as f32).sqrt()
}

fn make_line(dst: &mut [u8; MAX_COLS], region: Region, features: Features) {
    dst.fill(b' ');
    let mut text = Text { dst, at: 0 };
    // Follow the source's left contour in two-column code indentation steps.
    // Leave most of the row available for a single long statement.
    let left = (features.center - features.span * 0.5).max(0.0);
    let indent = (left * (region.width / 4).min(16) as f32 / 2.0).round() as usize * 2;
    for _ in 0..indent {
        text.bytes(b" ");
    }
    let kind = (region.row + region.pane * 3) % 6;
    text.bytes(match kind {
        0 => b"$ trace --row=",
        1 => b"const region_",
        2 => b"  patch[",
        3 => b"  scan_row(",
        4 => b"$ decode --row=",
        _ => b"  resolve(row=",
    });
    text.number(region.row as u32, 3);
    text.bytes(match kind {
        1 => b" = sample(",
        2 => b"] = rebuild(",
        3 => b", ",
        5 => b", ",
        _ => b" ",
    });
    text.bytes(b"x=");
    text.value(features.center);
    text.bytes(b", span=");
    text.value(features.span);
    text.bytes(b", luma=");
    text.value(features.light);
    text.bytes(b", edge=");
    text.value(features.edge);
    text.bytes(match kind {
        0 | 4 => b" /dev/video0 --follow",
        _ => b"); // source geometry",
    });
    if features.edge > 0.12 {
        text.bytes(b" [edge-lock]");
    }
}

struct Text<'a> {
    dst: &'a mut [u8],
    at: usize,
}
impl Text<'_> {
    fn bytes(&mut self, bytes: &[u8]) {
        let n = bytes.len().min(self.dst.len().saturating_sub(self.at));
        self.dst[self.at..self.at + n].copy_from_slice(&bytes[..n]);
        self.at += n;
    }
    fn number(&mut self, mut value: u32, digits: usize) {
        let mut buffer = [b'0'; 10];
        for byte in buffer[..digits].iter_mut().rev() {
            *byte += (value % 10) as u8;
            value /= 10;
        }
        self.bytes(&buffer[..digits]);
    }
    fn value(&mut self, value: f32) {
        // Quantized fields suppress tiny measurement jitter. The profile
        // threshold separately prevents typing in response to sensor noise.
        let quantized = (unit(value) * 64.0).round() / 64.0;
        let thousand = (quantized * 1000.0).round() as u32;
        self.number(thousand / 1000, 1);
        self.bytes(b".");
        self.number(thousand % 1000, 3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source(x: usize) -> [[f32; 4]; SAMPLE_COUNT] {
        let mut samples = [[0.03, 0.03, 0.03, 0.0]; SAMPLE_COUNT];
        for y in 12..20 {
            for col in x..x + 12 {
                samples[y * SAMPLE_COLS + col] = [0.9, 0.9, 0.9, 0.5];
            }
        }
        samples
    }
    fn settle(
        terminal: &mut ReactiveTerminal,
        layout: u8,
        samples: &[[f32; 4]; SAMPLE_COUNT],
        start: f64,
    ) {
        for frame in 1..=160 {
            terminal.update(
                128,
                36,
                layout,
                Beats(start + f64::from(frame) * 0.125),
                1.0,
                samples,
            );
        }
    }
    #[test]
    fn stationary_source_has_no_idle_animation() {
        let samples = source(8);
        let mut terminal = ReactiveTerminal::new();
        terminal.update(128, 36, 0, Beats(0.0), 1.0, &samples);
        let before = terminal.cells().to_vec();
        settle(&mut terminal, 0, &samples, 0.0);
        assert_eq!(before, terminal.cells());
        assert!(!terminal.cells().contains(&127));
        assert!(
            terminal
                .cells()
                .chunks(128)
                .all(|row| row[64..96].iter().any(|&c| c > 32))
        );
    }
    #[test]
    fn spatial_change_updates_local_rows_and_then_stops() {
        let mut terminal = ReactiveTerminal::new();
        terminal.update(128, 36, 0, Beats(0.0), 1.0, &source(4));
        let initial = terminal.cells().to_vec();
        settle(&mut terminal, 0, &source(44), 0.0);
        let final_cells = terminal.cells().to_vec();
        assert_ne!(initial, final_cells);
        for row in 0..36 {
            if !(12..20).contains(&row) {
                assert_eq!(
                    &initial[row * 128..(row + 1) * 128],
                    &final_cells[row * 128..(row + 1) * 128]
                );
            }
        }
        settle(&mut terminal, 0, &source(44), 20.0);
        assert_eq!(final_cells, terminal.cells());
    }
    #[test]
    fn edits_are_sparse_and_preserve_unchanged_text() {
        let mut terminal = ReactiveTerminal::new();
        terminal.update(128, 36, 0, Beats(0.0), 1.0, &source(4));
        let mut prior = terminal.cells().to_vec();
        let mut saw_cursor = false;
        for frame in 1..120 {
            terminal.update(128, 36, 0, Beats(f64::from(frame) / 32.0), 1.0, &source(44));
            let cursors = terminal.cells().iter().filter(|&&c| c == 127).count();
            assert!(cursors <= MAX_ACTIVE);
            saw_cursor |= cursors != 0;
            let changed = terminal
                .cells()
                .chunks(128)
                .zip(prior.chunks(128))
                .filter(|(a, b)| a != b)
                .count();
            assert!(changed <= MAX_ACTIVE + 1);
            prior.copy_from_slice(terminal.cells());
        }
        assert!(saw_cursor);
    }
    #[test]
    fn layouts_have_real_borders_and_single_has_no_subcolumns() {
        let mut terminal = ReactiveTerminal::new();
        for layout in 0..4 {
            terminal.update(128, 36, layout, Beats(0.0), 1.0, &source(4));
            assert!(terminal.cells().iter().all(|&c| (32..=127).contains(&c)));
            if layout == 0 {
                assert!(
                    terminal.lines[..terminal.line_count]
                        .iter()
                        .all(|l| l.region.width == 126)
                );
            }
            if layout == 1 || layout == 3 {
                assert_eq!(terminal.cells()[10 * 128 + 64], u32::from(b'|'));
            }
            if layout == 2 || layout == 3 {
                assert_eq!(terminal.cells()[18 * 128], u32::from(b'+'));
            }
        }
    }
    #[test]
    fn freeze_same_beat_resize_and_rewind_are_deterministic() {
        let samples = source(4);
        let mut terminal = ReactiveTerminal::new();
        terminal.update(128, 36, 0, Beats(0.0), 1.0, &samples);
        let initial = terminal.cells().to_vec();
        terminal.update(128, 36, 0, Beats(1.0), 0.0, &source(44));
        assert_eq!(initial, terminal.cells());
        terminal.update(128, 36, 0, Beats(1.0), 4.0, &source(44));
        assert_eq!(initial, terminal.cells());
        terminal.update(64, 20, 3, Beats(1.0), 1.0, &samples);
        assert_eq!(terminal.cells().len(), 64 * 20);
        terminal.update(128, 36, 0, Beats(0.0), 1.0, &samples);
        assert_eq!(initial, terminal.cells());
    }
    #[test]
    fn noise_below_threshold_does_not_trigger_typing() {
        let samples = source(4);
        let mut terminal = ReactiveTerminal::new();
        terminal.update(128, 36, 0, Beats(0.0), 1.0, &samples);
        let initial = terminal.cells().to_vec();
        let mut jitter = samples;
        for sample in &mut jitter {
            for channel in sample {
                *channel += 0.004;
            }
        }
        settle(&mut terminal, 0, &jitter, 0.0);
        assert_eq!(initial, terminal.cells());
    }
    #[test]
    fn invalid_values_and_tiny_layouts_stay_bounded() {
        let mut terminal = ReactiveTerminal::new();
        let samples = [[f32::NAN, f32::INFINITY, -1.0, f32::NEG_INFINITY]; SAMPLE_COUNT];
        terminal.update(
            u32::MAX,
            u32::MAX,
            3,
            Beats(f64::NAN),
            f32::INFINITY,
            &samples,
        );
        assert_eq!(terminal.cells().len(), MAX_COLS * MAX_ROWS);
        terminal.update(1, 1, 3, Beats(1.0), 1.0, &samples);
        assert_eq!(terminal.cells().len(), 1);
    }
}
