//! Continuity checks before capture and layer samples enter a shared analyzer.

use manifold_core::audio_features::AudioInputProblem;
use manifold_core::audio_stream::{AudioBlockStamp, AudioStreamRead};
use std::time::Instant;

const INPUT_SPAN_CAPACITY: usize = 4096;

#[derive(Clone, Copy)]
struct InputSpan {
    start_frame: usize,
    end_frame: usize,
    stamp: AudioBlockStamp,
}

pub(super) struct InputBatch {
    pub samples: Vec<f32>,
    pub interrupted: bool,
    pub drained_update: u64,
    next_frame: Option<u64>,
    format: Option<(u64, u32)>,
    channels: Option<usize>,
    spans: Vec<InputSpan>,
}

impl Default for InputBatch {
    fn default() -> Self {
        Self {
            samples: Vec::new(),
            interrupted: false,
            drained_update: 0,
            next_frame: None,
            format: None,
            channels: None,
            spans: Vec::with_capacity(INPUT_SPAN_CAPACITY),
        }
    }
}

impl InputBatch {
    pub fn drain_once(&mut self, update: u64, drain: impl FnOnce(&mut Self)) {
        if self.drained_update == update {
            return;
        }
        self.begin(update);
        drain(self);
    }

    pub fn begin(&mut self, update: u64) {
        self.samples.clear();
        self.spans.clear();
        self.interrupted = false;
        self.drained_update = update;
        self.channels = None;
    }

    pub fn sample_rate(&self) -> Option<u32> {
        self.format.map(|(_, rate)| rate)
    }

    /// Resolve a batch-local source-frame boundary against its source clock.
    /// An exclusive end boundary belongs to the preceding span, even if the
    /// next block is already available. Otherwise a hop's time could change
    /// depending on whether both blocks arrived in the same display update.
    pub fn source_time_at(&self, frame_offset: usize) -> Option<Instant> {
        if self.interrupted {
            return None;
        }
        let last = self.spans.last()?;
        if frame_offset > last.end_frame {
            return None;
        }
        let span = if frame_offset == 0 {
            self.spans.first()?
        } else {
            self.spans
                .iter()
                .find(|span| frame_offset > span.start_frame && frame_offset <= span.end_frame)?
        };
        let offset = frame_offset.checked_sub(span.start_frame)?;
        let source_frame = span
            .stamp
            .first_frame
            .checked_add(u64::try_from(offset).ok()?)?;
        AudioBlockStamp {
            first_frame: source_frame,
            ..span.stamp
        }
        .source_time()
    }

    pub fn unavailable(&mut self, mut report: impl FnMut(AudioInputProblem)) {
        if self.format.take().is_some() || self.next_frame.is_some() {
            self.interrupt(AudioInputProblem::SourceChanged, &mut report);
        }
        self.next_frame = None;
        self.samples.clear();
        self.spans.clear();
        self.channels = None;
    }

    pub fn consume(
        &mut self,
        read: AudioStreamRead,
        samples: &[f32],
        channels: usize,
        mut report: impl FnMut(AudioInputProblem),
    ) {
        match read {
            AudioStreamRead::Samples {
                stamp,
                samples: count,
            } => {
                if channels == 0 || !count.is_multiple_of(channels) {
                    self.interrupt(AudioInputProblem::InvalidInput, &mut report);
                    return;
                }
                if count == 0 || samples.len() != count {
                    self.interrupt(AudioInputProblem::InvalidInput, &mut report);
                    return;
                }
                if self.channels.is_some_and(|old| old != channels) {
                    self.interrupt(AudioInputProblem::InvalidInput, &mut report);
                    return;
                }
                let frame_count = count / channels;
                let Some(frame_count_u64) = u64::try_from(frame_count).ok() else {
                    self.interrupt(AudioInputProblem::InvalidInput, &mut report);
                    return;
                };
                let Some(end_frame) = stamp.first_frame.checked_add(frame_count_u64) else {
                    self.interrupt(AudioInputProblem::InvalidInput, &mut report);
                    return;
                };
                let Some(start_frame) = self
                    .samples
                    .len()
                    .checked_div(channels)
                    .filter(|_| self.samples.len().is_multiple_of(channels))
                else {
                    self.interrupt(AudioInputProblem::InvalidInput, &mut report);
                    return;
                };
                let Some(end_batch_frame) = start_frame.checked_add(frame_count) else {
                    self.interrupt(AudioInputProblem::InvalidInput, &mut report);
                    return;
                };
                self.channels = Some(channels);
                let format = (stamp.generation, stamp.sample_rate);
                if self.format.is_some_and(|old| old != format) {
                    self.interrupt(AudioInputProblem::FormatChanged, &mut report);
                }
                if self
                    .next_frame
                    .is_some_and(|expected| expected != stamp.first_frame)
                {
                    self.interrupt(AudioInputProblem::SourceChanged, &mut report);
                }
                self.format = Some(format);
                self.next_frame = Some(end_frame);
                if !self.interrupted {
                    if self.spans.len() == INPUT_SPAN_CAPACITY {
                        self.interrupt(AudioInputProblem::AnalysisOverflow, &mut report);
                        return;
                    }
                    self.samples.extend_from_slice(samples);
                    self.spans.push(InputSpan {
                        start_frame,
                        end_frame: end_batch_frame,
                        stamp,
                    });
                }
            }
            AudioStreamRead::Gap {
                first_frame,
                end_frame,
            } => {
                self.next_frame = Some(end_frame);
                self.interrupt(
                    AudioInputProblem::Gap {
                        first_frame,
                        end_frame,
                    },
                    &mut report,
                );
            }
            AudioStreamRead::InvalidInput => {
                self.interrupt(AudioInputProblem::InvalidInput, &mut report);
            }
        }
    }

    fn interrupt(
        &mut self,
        problem: AudioInputProblem,
        report: &mut impl FnMut(AudioInputProblem),
    ) {
        // A display batch can mix several sources. Discard this interrupted
        // batch rather than joining unrelated intervals or inventing alignment
        // for its surviving tail. The next update starts a fresh analyzer.
        self.interrupted = true;
        self.samples.clear();
        self.spans.clear();
        report(problem);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::audio_stream::{AudioBlockStamp, AudioClockAnchor, audio_stream};
    use std::time::Duration;

    fn sample(
        first_frame: u64,
        generation: u64,
        sample_rate: u32,
        samples: usize,
    ) -> AudioStreamRead {
        AudioStreamRead::Samples {
            stamp: AudioBlockStamp {
                first_frame,
                generation,
                sample_rate,
                clock: None,
            },
            samples,
        }
    }

    fn clocked_sample(
        first_frame: u64,
        generation: u64,
        sample_rate: u32,
        clock: Option<AudioClockAnchor>,
        samples: usize,
    ) -> AudioStreamRead {
        AudioStreamRead::Samples {
            stamp: AudioBlockStamp {
                first_frame,
                generation,
                sample_rate,
                clock,
            },
            samples,
        }
    }

    #[test]
    fn source_time_survives_partial_reads_and_display_batches() {
        let anchor = Instant::now();
        let clock = Some(AudioClockAnchor {
            instant: anchor,
            frame: 0,
        });
        let mut batch = InputBatch::default();
        batch.begin(1);
        batch.consume(clocked_sample(0, 0, 3, clock, 2), &[1., 2.], 2, |_| {
            panic!("valid first span")
        });
        batch.consume(clocked_sample(1, 0, 3, clock, 2), &[3., 4.], 2, |_| {
            panic!("valid second span")
        });
        assert_eq!(batch.source_time_at(0), Some(anchor));
        assert_eq!(
            batch.source_time_at(1),
            anchor.checked_add(Duration::new(0, 333_333_333))
        );
        assert_eq!(
            batch.source_time_at(2),
            anchor.checked_add(Duration::new(0, 666_666_666))
        );
        assert_eq!(batch.source_time_at(3), None);

        batch.begin(2);
        batch.consume(
            clocked_sample(2, 0, 3, clock, 4),
            &[5., 6., 7., 8.],
            2,
            |_| panic!("valid next display batch"),
        );
        assert_eq!(
            batch.source_time_at(0),
            anchor.checked_add(Duration::new(0, 666_666_666))
        );
        assert_eq!(
            batch.source_time_at(2),
            anchor.checked_add(Duration::new(1, 333_333_333))
        );
    }

    #[test]
    fn source_time_uses_preceding_span_at_every_exclusive_boundary() {
        let first_anchor = Instant::now();
        let second_anchor = first_anchor + Duration::from_secs(10);
        let mut batch = InputBatch::default();
        batch.begin(1);
        batch.consume(
            clocked_sample(
                0,
                0,
                1,
                Some(AudioClockAnchor {
                    instant: first_anchor,
                    frame: 0,
                }),
                1,
            ),
            &[1.],
            1,
            |_| panic!("valid first span"),
        );
        batch.consume(
            clocked_sample(
                1,
                0,
                1,
                Some(AudioClockAnchor {
                    instant: second_anchor,
                    frame: 1,
                }),
                1,
            ),
            &[2.],
            1,
            |_| panic!("valid second span"),
        );
        assert_eq!(
            batch.source_time_at(1),
            first_anchor.checked_add(Duration::from_secs(1))
        );
        assert_eq!(
            batch.source_time_at(2),
            second_anchor.checked_add(Duration::from_secs(1))
        );
    }

    #[test]
    fn missing_clocks_never_derive_a_drain_time() {
        let mut batch = InputBatch::default();
        batch.begin(1);
        batch.consume(sample(0, 0, 48_000, 2), &[1., 2.], 1, |_| {
            panic!("valid unclocked span")
        });
        assert_eq!(batch.source_time_at(0), None);
        assert_eq!(batch.source_time_at(2), None);
    }

    #[test]
    fn source_or_rate_reset_discards_timestamp_spans() {
        let anchor = Instant::now();
        let mut batch = InputBatch::default();
        let mut issues = Vec::new();
        batch.begin(1);
        batch.consume(
            clocked_sample(
                0,
                0,
                48_000,
                Some(AudioClockAnchor {
                    instant: anchor,
                    frame: 0,
                }),
                1,
            ),
            &[1.],
            1,
            |problem| issues.push(problem),
        );
        batch.begin(2);
        batch.consume(
            clocked_sample(
                0,
                1,
                44_100,
                Some(AudioClockAnchor {
                    instant: anchor,
                    frame: 0,
                }),
                1,
            ),
            &[2.],
            1,
            |problem| issues.push(problem),
        );
        assert!(batch.interrupted);
        assert!(batch.samples.is_empty());
        assert_eq!(batch.source_time_at(0), None);
        assert_eq!(
            issues,
            [
                AudioInputProblem::FormatChanged,
                AudioInputProblem::SourceChanged
            ]
        );
    }

    #[test]
    fn span_overflow_reports_analysis_overflow_without_partial_coverage() {
        let mut batch = InputBatch::default();
        let mut issues = Vec::new();
        batch.begin(1);
        for frame in 0..=INPUT_SPAN_CAPACITY {
            batch.consume(
                sample(frame as u64, 0, 48_000, 1),
                &[frame as f32],
                1,
                |problem| issues.push(problem),
            );
        }
        assert!(batch.interrupted);
        assert!(batch.samples.is_empty());
        assert_eq!(batch.source_time_at(0), None);
        assert_eq!(issues, [AudioInputProblem::AnalysisOverflow]);
        assert_eq!(batch.spans.capacity(), INPUT_SPAN_CAPACITY);
    }

    #[test]
    fn malformed_reads_fail_before_retaining_a_span() {
        let cases = [
            (
                AudioBlockStamp {
                    first_frame: 0,
                    generation: 0,
                    sample_rate: 48_000,
                    clock: None,
                },
                2,
                2,
                vec![1.],
            ),
            (
                AudioBlockStamp {
                    first_frame: 0,
                    generation: 0,
                    sample_rate: 48_000,
                    clock: None,
                },
                1,
                2,
                vec![1.],
            ),
            (
                AudioBlockStamp {
                    first_frame: 0,
                    generation: 0,
                    sample_rate: 48_000,
                    clock: None,
                },
                1,
                0,
                vec![1.],
            ),
            (
                AudioBlockStamp {
                    first_frame: u64::MAX,
                    generation: 0,
                    sample_rate: 48_000,
                    clock: None,
                },
                1,
                1,
                vec![1.],
            ),
        ];
        for (stamp, count, channels, input) in cases {
            let mut batch = InputBatch::default();
            let mut issues = Vec::new();
            batch.begin(1);
            batch.consume(
                AudioStreamRead::Samples {
                    stamp,
                    samples: count,
                },
                &input,
                channels,
                |problem| issues.push(problem),
            );
            assert!(batch.interrupted);
            assert!(batch.samples.is_empty());
            assert!(batch.spans.is_empty());
            assert_eq!(issues, [AudioInputProblem::InvalidInput]);
        }
    }

    #[test]
    fn two_sends_reuse_one_layer_drain() {
        let (mut writer, mut reader) = audio_stream(1, 8, 4, 48_000);
        writer.push_interleaved(&[0.2, -0.4, 0.6]);
        let mut batch = InputBatch::default();
        let mut drain_count = 0;
        let mut send_inputs = Vec::new();
        for _send in 0..2 {
            batch.drain_once(1, |batch| {
                drain_count += 1;
                let mut scratch = [0.; 8];
                while let Some(read) = reader.read(&mut scratch) {
                    let AudioStreamRead::Samples { samples, .. } = read else {
                        panic!("complete source fixture");
                    };
                    batch.consume(read, &scratch[..samples], 1, |_| {
                        panic!("continuous source")
                    });
                }
            });
            send_inputs.push(batch.samples.clone());
        }
        assert_eq!(drain_count, 1);
        assert_eq!(send_inputs, [vec![0.2, -0.4, 0.6], vec![0.2, -0.4, 0.6]]);
        batch.drain_once(2, |_| {
            drain_count += 1;
        });
        assert_eq!(drain_count, 2);
        assert!(batch.samples.is_empty());
    }

    #[test]
    fn uninterrupted_reads_keep_all_samples_across_display_partitions() {
        for partition in [1, 7, 32] {
            let mut batch = InputBatch::default();
            let mut collected = Vec::new();
            for update in 1..=3 {
                batch.begin(update);
                for first in ((update - 1) * 64..update * 64).step_by(partition) {
                    let count = partition.min((update * 64 - first) as usize);
                    let input: Vec<_> = (first..first + count as u64).map(|v| v as f32).collect();
                    batch.consume(sample(first, 0, 48_000, count), &input, 1, |_| {
                        panic!("unexpected discontinuity")
                    });
                }
                collected.extend_from_slice(&batch.samples);
            }
            assert_eq!(collected, (0..192).map(|v| v as f32).collect::<Vec<_>>());
        }
    }

    #[test]
    fn overflow_discards_the_interrupted_mix_without_joining_its_tail() {
        let (mut writer, mut reader) = audio_stream(1, 2, 8, 48_000);
        let mut batch = InputBatch::default();
        let mut issues = Vec::new();
        let mut scratch = [0.; 8];
        batch.begin(1);
        writer.push_interleaved(&[1., 2., 3., 4.]);
        while let Some(read) = reader.read(&mut scratch) {
            let count = if let AudioStreamRead::Samples { samples, .. } = read {
                samples
            } else {
                0
            };
            batch.consume(read, &scratch[..count], 1, |problem| issues.push(problem));
        }
        writer.push_interleaved(&[5., 6.]);
        let read = reader.read(&mut scratch).unwrap();
        batch.consume(read, &scratch[..2], 1, |problem| issues.push(problem));
        assert!(batch.interrupted);
        assert!(batch.samples.is_empty());
        assert_eq!(
            issues,
            [AudioInputProblem::Gap {
                first_frame: 2,
                end_frame: 4
            }]
        );
        batch.begin(2);
        writer.push_interleaved(&[7.]);
        batch.consume(reader.read(&mut scratch).unwrap(), &scratch[..1], 1, |_| {
            panic!("gap was already consumed")
        });
        assert_eq!(batch.samples, [7.]);
        assert!(!batch.interrupted);
    }

    #[test]
    fn removed_layer_does_not_leave_a_stale_rate_or_feature_source() {
        let mut batch = InputBatch::default();
        batch.begin(1);
        batch.consume(sample(0, 0, 48_000, 2), &[1., 2.], 1, |_| {
            panic!("first source")
        });
        batch.begin(2);
        let mut issues = Vec::new();
        batch.unavailable(|problem| issues.push(problem));
        assert!(batch.interrupted);
        assert!(batch.samples.is_empty());
        assert_eq!(batch.sample_rate(), None);
        assert_eq!(issues, [AudioInputProblem::SourceChanged]);
        batch.begin(3);
        batch.unavailable(|problem| issues.push(problem));
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn source_restart_and_rate_change_invalidate_old_analysis() {
        let mut batch = InputBatch::default();
        let mut issues = Vec::new();
        batch.begin(1);
        batch.consume(sample(0, 0, 44_100, 2), &[1., 2.], 1, |p| issues.push(p));
        batch.begin(2);
        batch.consume(sample(0, 0, 44_100, 1), &[3.], 1, |p| issues.push(p));
        assert_eq!(issues, [AudioInputProblem::SourceChanged]);
        batch.begin(3);
        batch.consume(sample(1, 1, 48_000, 1), &[4.], 1, |p| issues.push(p));
        assert_eq!(issues.last(), Some(&AudioInputProblem::FormatChanged));
        assert!(batch.samples.is_empty());
    }
}
