//! Continuity checks before capture and layer samples enter a shared analyzer.

use manifold_core::audio_features::AudioInputProblem;
use manifold_core::audio_stream::AudioStreamRead;

#[derive(Default)]
pub(super) struct InputBatch {
    pub samples: Vec<f32>,
    pub interrupted: bool,
    pub drained_update: u64,
    next_frame: Option<u64>,
    format: Option<(u64, u32)>,
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
        self.interrupted = false;
        self.drained_update = update;
    }

    pub fn sample_rate(&self) -> Option<u32> {
        self.format.map(|(_, rate)| rate)
    }

    pub fn unavailable(&mut self, mut report: impl FnMut(AudioInputProblem)) {
        if self.format.take().is_some() || self.next_frame.is_some() {
            self.interrupt(AudioInputProblem::SourceChanged, &mut report);
        }
        self.next_frame = None;
        self.samples.clear();
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
                self.next_frame = Some(stamp.first_frame + (count / channels) as u64);
                if !self.interrupted {
                    self.samples.extend_from_slice(samples);
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
        report(problem);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::audio_stream::{AudioBlockStamp, audio_stream};

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
            },
            samples,
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
