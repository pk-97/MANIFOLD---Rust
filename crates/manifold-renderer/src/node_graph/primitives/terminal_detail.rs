//! Bounded character edits driven by the fine source grid. Each cell keeps a
//! measurement baseline and a short, finite transition; there is no idle clock.
use super::terminal_analysis::{DETAIL_COLS, DETAIL_COUNT, DETAIL_ROWS};
use manifold_core::Beats;

const CAPACITY: usize = 640 * 135;

#[derive(Clone, Copy, Default)]
struct Cell {
    reference: [f32; 4],
    base: u32,
    shown: u32,
    target: u32,
    age: f32,
    cycling: bool,
    eligible: bool,
}

pub(super) struct DetailFrame {
    pub columns: usize,
    pub rows: usize,
    pub layout: u8,
    pub beat: Beats,
    pub activity: f32,
    pub amount: f32,
}

pub(super) struct TerminalDetail {
    states: Box<[Cell]>,
    cells: Box<[u32]>,
    classes: Box<[u8]>,
    dimensions: (usize, usize, u8),
    last_beat: Option<f64>,
}

impl TerminalDetail {
    pub(super) fn new() -> Self {
        Self {
            states: vec![Cell::default(); CAPACITY].into_boxed_slice(),
            cells: vec![32; CAPACITY].into_boxed_slice(),
            classes: vec![0; CAPACITY].into_boxed_slice(),
            dimensions: (0, 0, 0),
            last_beat: None,
        }
    }

    pub(super) fn reset(&mut self) {
        self.last_beat = None;
    }

    pub(super) fn update(
        &mut self,
        base: &[u32],
        samples: &[[f32; 4]; DETAIL_COUNT],
        frame: DetailFrame,
    ) -> &[u32] {
        let count = base.len().min(CAPACITY);
        let dimensions = (frame.columns, frame.rows, frame.layout);
        let beat = if frame.beat.0.is_finite() {
            frame.beat.0.max(0.0)
        } else {
            self.last_beat.unwrap_or(0.0)
        };
        let reset =
            self.last_beat.is_none_or(|previous| beat < previous) || dimensions != self.dimensions;
        let delta = (beat - self.last_beat.unwrap_or(beat)).clamp(0.0, 8.0) as f32;
        self.last_beat = Some(beat);
        self.dimensions = dimensions;
        // Match the line scheduler: freeze does not accrue time to catch up.
        if !reset && (delta == 0.0 || !frame.activity.is_finite() || frame.activity <= 0.0) {
            return &self.cells[..count];
        }
        let amount = unit(frame.amount);
        let step = delta * frame.activity.clamp(0.0, 4.0);
        let threshold = 0.12 - amount * 0.09;
        for (y, row) in base[..count].chunks(frame.columns.max(1)).enumerate() {
            let start = y * frame.columns;
            let classes = &mut self.classes[start..start + row.len()];
            classify(row, classes);
            for (x, &code) in row.iter().enumerate() {
                let index = start + x;
                let state = &mut self.states[index];
                let eligible = classes[x] != 0
                    && !border(x, y, &frame)
                    && ((hash(index as u32) % 1000) as f32) < amount * 1000.0;
                // Most cells are words or whitespace. Only data cells need
                // fine-grid interpolation; eligibility changes rebaseline.
                let sample = if eligible {
                    measurement(samples, x, y, frame.columns, frame.rows)
                } else {
                    [0.0; 4]
                };
                if reset || code != state.base || amount == 0.0 || !eligible || !state.eligible {
                    *state = Cell {
                        reference: sample,
                        base: code,
                        shown: code,
                        target: code,
                        eligible,
                        ..Cell::default()
                    };
                } else {
                    let change = sample
                        .iter()
                        .zip(state.reference)
                        .map(|(a, b)| (a - b).abs())
                        .fold(0.0_f32, f32::max);
                    let alphabet = alphabet(classes[x]);
                    if change >= threshold {
                        state.reference = sample;
                        let signal =
                            sample
                                .iter()
                                .enumerate()
                                .fold(index as u32, |seed, (i, v)| {
                                    hash(
                                        seed ^ ((v * 31.0).round() as u32)
                                            .rotate_left(i as u32 * 7),
                                    )
                                });
                        let target = u32::from(alphabet[signal as usize % alphabet.len()]);
                        if target != state.target {
                            state.target = target;
                            state.age = 0.0;
                            state.cycling = true;
                        }
                    }
                    if state.cycling {
                        state.age += step;
                        if state.age >= 0.45 {
                            state.shown = state.target;
                            state.cycling = false;
                        } else {
                            let phase = (state.age * 9.0).floor() as u32;
                            state.shown = u32::from(
                                alphabet[hash(index as u32 ^ phase ^ state.target) as usize
                                    % alphabet.len()],
                            );
                        }
                    }
                }
                self.cells[index] = state.shown;
            }
        }
        &self.cells[..count]
    }
}

fn unit(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}
fn hash(mut value: u32) -> u32 {
    value ^= value >> 16;
    value = value.wrapping_mul(0x7feb_352d);
    value ^= value >> 15;
    value.wrapping_mul(0x846c_a68b) ^ (value >> 16)
}

fn alphabet(class: u8) -> &'static [u8] {
    match class {
        1 => b"0123456789",
        2 => b"0123456789abcdef",
        3 => b".,:;",
        _ => b"=<>",
    }
}

fn classify(row: &[u32], classes: &mut [u8]) {
    classes.fill(0);
    let mut at = 0;
    while at < row.len() {
        let c = row[at];
        if c <= 127 && (c as u8).is_ascii_alphanumeric() {
            let start = at;
            while at < row.len() && row[at] <= 127 && (row[at] as u8).is_ascii_alphanumeric() {
                at += 1;
            }
            let token = &row[start..at];
            let decimal = token.iter().all(|&c| (c as u8).is_ascii_digit());
            let hex_start = usize::from(token.starts_with(&[48, 120])) * 2;
            let hex = token.len() >= 4
                && (hex_start == 2 || token.iter().any(|&c| (c as u8).is_ascii_alphabetic()))
                && token[hex_start..]
                    .iter()
                    .all(|&c| (c as u8).is_ascii_hexdigit())
                && token.iter().any(|&c| (c as u8).is_ascii_digit());
            for i in start..at {
                classes[i] = if hex {
                    if i >= start + hex_start { 2 } else { 0 }
                } else if decimal {
                    1
                } else {
                    0
                };
            }
        } else {
            // Punctuation next to data may change; syntax braces, prompts,
            // paths, operators and word separators remain readable.
            if matches!(c, 44 | 46 | 58 | 59) && at > 0 && (row[at - 1] as u8).is_ascii_digit() {
                classes[at] = 3;
            }
            at += 1;
        }
    }
}

fn border(x: usize, y: usize, frame: &DetailFrame) -> bool {
    frame.layout != 0
        && (x == 0
            || x + 1 == frame.columns
            || y == 0
            || y + 1 == frame.rows
            || ((frame.layout == 1 || frame.layout == 3) && x == frame.columns / 2)
            || ((frame.layout == 2 || frame.layout == 3) && y == frame.rows / 2))
}

fn measurement(
    samples: &[[f32; 4]; DETAIL_COUNT],
    x: usize,
    y: usize,
    columns: usize,
    rows: usize,
) -> [f32; 4] {
    let sx = ((x as f32 + 0.5) * DETAIL_COLS as f32 / columns.max(1) as f32 - 0.5)
        .clamp(0.0, (DETAIL_COLS - 1) as f32);
    let sy = ((y as f32 + 0.5) * DETAIL_ROWS as f32 / rows.max(1) as f32 - 0.5)
        .clamp(0.0, (DETAIL_ROWS - 1) as f32);
    let ix = sx as usize;
    let iy = sy as usize;
    let right = (ix + 1).min(DETAIL_COLS - 1);
    let down = (iy + 1).min(DETAIL_ROWS - 1);
    std::array::from_fn(|c| {
        let top = unit(samples[iy * DETAIL_COLS + ix][c]) * (1.0 - sx.fract())
            + unit(samples[iy * DETAIL_COLS + right][c]) * sx.fract();
        let bottom = unit(samples[down * DETAIL_COLS + ix][c]) * (1.0 - sx.fract())
            + unit(samples[down * DETAIL_COLS + right][c]) * sx.fract();
        top * (1.0 - sy.fract()) + bottom * sy.fract()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(beat: f64, amount: f32, activity: f32) -> DetailFrame {
        DetailFrame {
            columns: 64,
            rows: 4,
            layout: 0,
            beat: Beats(beat),
            activity,
            amount,
        }
    }
    fn text() -> Vec<u32> {
        let mut row = b"$ decode_region buffer=0x123456789abcdef0 size=1234567890;".to_vec();
        row.resize(64, b' ');
        row.repeat(4).into_iter().map(u32::from).collect()
    }
    #[test]
    fn local_detail_changes_data_only_then_settles() {
        let base = text();
        let mut samples: Box<[[f32; 4]; DETAIL_COUNT]> = vec![[0.2; 4]; DETAIL_COUNT]
            .into_boxed_slice()
            .try_into()
            .unwrap();
        let mut detail = TerminalDetail::new();
        assert_eq!(detail.update(&base, &samples, frame(0.0, 1.0, 1.0)), base);
        for y in 0..DETAIL_ROWS / 4 {
            for x in DETAIL_COLS / 3..DETAIL_COLS * 3 / 4 {
                samples[y * DETAIL_COLS + x] = [0.8, 0.4, 0.1, 0.7];
            }
        }
        let during = detail
            .update(&base, &samples, frame(0.125, 1.0, 1.0))
            .to_vec();
        assert_ne!(during, base);
        assert_eq!(&during[64..], &base[64..]);
        assert_eq!(&during[..22], &base[..22], "command words stay readable");
        let settled = detail
            .update(&base, &samples, frame(2.0, 1.0, 1.0))
            .to_vec();
        assert_eq!(
            detail.update(&base, &samples, frame(4.0, 1.0, 1.0)),
            settled
        );
        assert_eq!(
            detail.update(&base, &samples, frame(5.0, 0.0, 1.0)),
            base,
            "zero bypasses detail edits"
        );
    }
    #[test]
    fn noise_freeze_rewind_and_same_beat_hold() {
        let base = text();
        let a: Box<[[f32; 4]; DETAIL_COUNT]> = vec![[0.2; 4]; DETAIL_COUNT]
            .into_boxed_slice()
            .try_into()
            .unwrap();
        let b: Box<[[f32; 4]; DETAIL_COUNT]> = vec![[0.8; 4]; DETAIL_COUNT]
            .into_boxed_slice()
            .try_into()
            .unwrap();
        let mut detail = TerminalDetail::new();
        detail.update(&base, &a, frame(0.0, 1.0, 1.0));
        let noise: Box<[[f32; 4]; DETAIL_COUNT]> = vec![[0.204; 4]; DETAIL_COUNT]
            .into_boxed_slice()
            .try_into()
            .unwrap();
        assert_eq!(detail.update(&base, &noise, frame(0.1, 1.0, 1.0)), base);
        assert_eq!(detail.update(&base, &b, frame(0.2, 1.0, 0.0)), base);
        assert_eq!(detail.update(&base, &b, frame(0.2, 1.0, 1.0)), base);
        let reacting = detail.update(&base, &b, frame(0.3, 1.0, 1.0)).to_vec();
        assert_ne!(reacting, base);
        assert_eq!(detail.update(&base, &a, frame(1.0, 1.0, 0.0)), reacting);
        assert_eq!(detail.update(&base, &a, frame(0.0, 1.0, 1.0)), base);
    }

    #[test]
    fn digits_inside_command_names_and_identifiers_stay_readable() {
        let mut row = b"$ sha256sum buffer32 r12 0x12345678 1234567890".to_vec();
        row.resize(64, b' ');
        let base: Vec<u32> = row.repeat(4).into_iter().map(u32::from).collect();
        let mut samples: Box<[[f32; 4]; DETAIL_COUNT]> = vec![[0.2; 4]; DETAIL_COUNT]
            .into_boxed_slice()
            .try_into()
            .unwrap();
        let mut detail = TerminalDetail::new();
        detail.update(&base, &samples, frame(0.0, 1.0, 1.0));
        samples.fill([0.8, 0.5, 0.1, 0.6]);
        let edited = detail.update(&base, &samples, frame(0.125, 1.0, 1.0));
        assert_ne!(edited, base, "data characters react");
        for (before, after) in base.chunks(64).zip(edited.chunks(64)) {
            assert_eq!(
                &before[..26],
                &after[..26],
                "command, identifiers and hex prefix stay intact"
            );
        }
    }
}
